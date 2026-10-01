//! 断句：把一段一口气写完的话，切成人在手机上会分几次发出的那几条。
//!
//! 模型写出来的是一整段，群友写出来的是三条。差别不在内容，在换气——一个意思讲完
//! 就先发出去，下一个意思重新起一条。这里只做这一件事：找出那段话里本来就有的换气
//! 处，按剩下的消息额度切开。
//!
//! 判断只看形状不看语义，所以宁可少切：模型自己排过版的（带换行的答疑、清单、代码）
//! 一律原样发出，短到本来就只值一条的也不动。切点有三种，强弱不同：
//!
//! 1. **句末标点**（`。！？…`）——最硬的换气，标点跟着上一段走。
//! 2. **汉字之间的空格**——这个人格用空格代替标点停顿，那正是它自己的断句意图；
//!    两边都必须是汉字，`Pro 比 Flash` 这种词间空格不算。
//! 3. **句中逗号**——最弱，切的时候逗号本身丢掉：分两条发的人不会把逗号留在句尾。
//!
//! 切几刀由长度定、由额度封顶，落在哪一刀由「离等分点多近 + 这处换气有多硬」决定。

/// 切出来的一段至少这么长。再短就不像换气，像手滑。
const MIN_FRAGMENT: usize = 4;

/// 句末标点：切在它后面，它跟着上一段。
const SENTENCE_END: [char; 6] = ['。', '！', '？', '…', '!', '?'];

/// 句末标点后面可能还跟着的收尾符号，一并留给上一段。
const CLOSERS: [char; 9] = ['」', '』', '”', '’', '）', ')', '】', '》', '"'];

/// 句中停顿：切在它前面，它本身不要。
const CLAUSE_END: [char; 4] = ['，', '；', ',', ';'];

/// 一处换气。`end` 之前归上一段，`resume` 之后归下一段，中间的空格与逗号丢掉。
#[derive(Clone, Copy, Debug)]
struct Cut {
    end: usize,
    resume: usize,
    weight: i64,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// 把一段话按换气处切开，最多 `budget` 条，每条大约 `target` 个字。
///
/// 不该切的时候原样返回一条——调用方拿到的永远是「要发出去的几条」，
/// 不必再判断这次到底切没切。
pub(crate) fn split(text: &str, budget: usize, target: usize) -> Vec<String> {
    split_protected(text, budget, target, &[])
}

/// 同 [`split`]，但 `protected` 里给出的字符区间（半开，按字符计）内不许下刀。
///
/// 用来护住 `[at:…]`、`[img:…]` 这类标记：token 里的逗号或空格看着像换气处，
/// 真切下去会把标记截成两半，那条消息就变成另一个意思了。护住的只是切口，
/// 标记之外该切照切——带标记的长句照样是几条短消息。
pub(crate) fn split_protected(
    text: &str,
    budget: usize,
    target: usize,
    protected: &[std::ops::Range<usize>],
) -> Vec<String> {
    let whole = || vec![text.trim().to_string()];
    let trimmed = text.trim();
    // 带换行说明模型自己排过版了，那是它的意思，不要替它重排。
    if budget <= 1 || target == 0 || trimmed.is_empty() || trimmed.contains('\n') {
        return whole();
    }
    // trim 掉的前导空白会让下标整体前移，护住区间跟着挪。
    let lead = text.chars().count() - text.trim_start().chars().count();
    let shield: Vec<std::ops::Range<usize>> = protected
        .iter()
        .map(|range| {
            let start = range.start.saturating_sub(lead);
            let end = range.end.saturating_sub(lead).max(start);
            start..end
        })
        .collect();
    let chars: Vec<char> = trimmed.chars().collect();
    // 四舍五入：一条半的长度才值得切成两条，免得把一句寻常的话劈成两半。
    let want = ((chars.len() + target / 2) / target).min(budget);
    if want <= 1 {
        return whole();
    }
    let cuts: Vec<Cut> = candidates(&chars)
        .into_iter()
        .filter(|cut| !shielded(cut, &shield))
        .collect();
    let mut chosen: Vec<Cut> = Vec::new();
    for index in 1..want {
        let ideal = (chars.len() * index / want) as i64;
        let Some(best) = cuts
            .iter()
            .filter(|cut| fits(&chosen, cut, chars.len()))
            // 离等分点越近越好，换气越硬越好；一处硬换气值得多走四个字去够。
            .min_by_key(|cut| (cut.end as i64 - ideal).abs() - cut.weight * 4)
        else {
            break;
        };
        chosen.push(*best);
        chosen.sort_by_key(|cut| cut.end);
    }
    if chosen.is_empty() {
        return whole();
    }
    let mut pieces = Vec::with_capacity(chosen.len() + 1);
    let mut start = 0;
    for cut in &chosen {
        pieces.push(chars[start..cut.end].iter().collect::<String>());
        start = cut.resume;
    }
    pieces.push(chars[start..].iter().collect::<String>());
    let pieces: Vec<String> = pieces
        .into_iter()
        .map(|piece| piece.trim().to_string())
        .filter(|piece| !piece.is_empty())
        .collect();
    if pieces.is_empty() { whole() } else { pieces }
}

/// 找出所有换气处。
fn candidates(chars: &[char]) -> Vec<Cut> {
    let total = chars.len();
    let mut cuts = Vec::new();
    let mut index = 0;
    while index < total {
        let current = chars[index];
        if SENTENCE_END.contains(&current) {
            let mut end = index + 1;
            while end < total && (SENTENCE_END.contains(&chars[end]) || CLOSERS.contains(&chars[end]))
            {
                end += 1;
            }
            let mut resume = end;
            while resume < total && chars[resume] == ' ' {
                resume += 1;
            }
            if resume < total {
                cuts.push(Cut {
                    end,
                    resume,
                    weight: 3,
                });
            }
            index = end;
            continue;
        }
        if current == ' '
            && index > 0
            && index + 1 < total
            && is_cjk(chars[index - 1])
            && is_cjk(chars[index + 1])
        {
            cuts.push(Cut {
                end: index,
                resume: index + 1,
                weight: 2,
            });
        }
        if CLAUSE_END.contains(&current) {
            let mut resume = index + 1;
            while resume < total && chars[resume] == ' ' {
                resume += 1;
            }
            if resume < total {
                cuts.push(Cut {
                    end: index,
                    resume,
                    weight: 1,
                });
            }
        }
        index += 1;
    }
    cuts
}

/// 这一刀会不会切进被护住的标记里。
///
/// 切口在 `end`（上一段末字的后一位）与 `resume`（下一段首字）之间；把
/// `[end-1, resume]` 这段缝拿去和每个护住区间比对，沾上就不能切。
fn shielded(cut: &Cut, shield: &[std::ops::Range<usize>]) -> bool {
    let band_start = cut.end.saturating_sub(1);
    let band_end = cut.resume;
    shield
        .iter()
        .any(|range| band_start < range.end && band_end >= range.start)
}

/// 这一刀会不会切出一个太短的碎片。
fn fits(chosen: &[Cut], cut: &Cut, total: usize) -> bool {
    if cut.end < MIN_FRAGMENT || total.saturating_sub(cut.resume) < MIN_FRAGMENT {
        return false;
    }
    chosen.iter().all(|other| {
        if cut.end > other.end {
            cut.end.saturating_sub(other.resume) >= MIN_FRAGMENT
        } else {
            other.end.saturating_sub(cut.resume) >= MIN_FRAGMENT
        }
    })
}
