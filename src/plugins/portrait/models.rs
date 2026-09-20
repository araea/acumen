//! 人格画像的「投影层」：两套把行为往既有框架上做的读数——MBTI 四轴与九型人格核心。
//!
//! 为什么是这两套，而不是更多：人格框架彼此高度重叠。MBTI 的内外倾、直觉实感、思考情感
//! 就是大五的外向性、开放性、宜人性；荣格的八个认知功能又是 MBTI 四轴背后的同一份数据
//! 换一种拆法。把它们一起摆上来，看似「多维分析」，其实是同一份证据讲了四遍，还更唬人。
//!
//! 所以只留两把**能各答一个别的问题**的尺子：
//! - **MBTI 四轴光谱**答「他往世界哪一侧使劲」——气质与风格，可量化、可讨论；
//! - **九型人格核心**答「他为什么这样使力」——动机与恐惧，这是 MBTI 结构上给不出的那一问。
//!
//! 两把都是尺子，不是判决。这里只做三件事：定义两套框架（轴的两极、九型的核心理念）、
//! 解析与归一（模型偶尔把分数写成 130、把九型写成 12，夹回合法区间）、取景（把数字翻成
//! 卡片要的字与百分比）。这里不写色值与字号，只给排版用的语义。

use serde::Deserialize;

// ==================== MBTI 四轴 ====================

/// 四轴各持一个 -100..100 的整数：正 = 靠向大写那一极（E/S/T/J），负 = 靠向小写那一极
/// （I/N/F/P），绝对值 = 强度。用符号而不是一串四字母存，是为了能画出一条从中间往一侧
/// 填的光谱：四字母只说得清站在哪边，说不到离中线多远。0 附近的人画出来是一条贴中的
/// 短线——那才是诚实的，多数人本就贴近中线。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Mbti {
    #[serde(default, alias = "EI", alias = "ei", alias = "能量", alias = "attitude")]
    pub energy: i32,
    #[serde(default, alias = "SN", alias = "sn", alias = "认知", alias = "perceiving")]
    pub perceiving: i32,
    #[serde(default, alias = "TF", alias = "tf", alias = "判断", alias = "deciding")]
    pub deciding: i32,
    #[serde(default, alias = "JP", alias = "jp", alias = "生活", alias = "lifestyle")]
    pub lifestyle: i32,
}

impl Mbti {
    /// 四条轴按经典次序 E/I → S/N → T/F → J/P。卡片照这个顺序画。
    pub fn axes(&self) -> [(&'static str, &'static str, i32); 4] {
        [
            ("E", "I", self.energy),
            ("S", "N", self.perceiving),
            ("T", "F", self.deciding),
            ("J", "P", self.lifestyle),
        ]
    }

    /// 由四轴推出的参考码，如 `INTJ`。0 与正取大写那一极。它是光谱的副产品，不是结论。
    pub fn code(&self) -> String {
        self.axes()
            .iter()
            .map(|(a, b, score)| if *score >= 0 { *a } else { *b })
            .collect()
    }

    /// 夹回 -100..100。
    pub fn sanitized(self) -> Self {
        Self {
            energy: clamp_axis(self.energy),
            perceiving: clamp_axis(self.perceiving),
            deciding: clamp_axis(self.deciding),
            lifestyle: clamp_axis(self.lifestyle),
        }
    }

    /// 参考码的常称，如「建筑师」。公开通行的别称，只为群里叫得出名字、聊得起来。
    pub fn epithet(&self) -> &'static str {
        epithet_of(&self.code())
    }
}

/// 一条轴靠近某一极的百分比（0..100），中线 50。
///
/// +80 → 偏大写极 90%；-30 → 偏小写极 65%；0 → 50。
pub fn axis_lean(score: i32) -> u8 {
    (50 + clamp_axis(score) / 2).clamp(0, 100) as u8
}

fn clamp_axis(value: i32) -> i32 {
    value.clamp(-100, 100)
}

/// 单个字母的中文极名，画在轴两端。
pub fn pole_short(pole: char) -> &'static str {
    match pole {
        'E' => "外向",
        'I' => "内向",
        'S' => "实感",
        'N' => "直觉",
        'T' => "思考",
        'F' => "情感",
        'J' => "判断",
        'P' => "知觉",
        _ => "",
    }
}

/// 十六型各自的常称。
fn epithet_of(code: &str) -> &'static str {
    match code {
        "INTJ" => "建筑师",
        "INTP" => "逻辑学家",
        "ENTJ" => "指挥官",
        "ENTP" => "辩论家",
        "INFJ" => "提倡者",
        "INFP" => "调停者",
        "ENFJ" => "主人公",
        "ENFP" => "竞选者",
        "ISTJ" => "物流师",
        "ISFJ" => "守卫者",
        "ESTJ" => "总经理",
        "ESFJ" => "执政官",
        "ISTP" => "鉴赏家",
        "ISFP" => "探险家",
        "ESTP" => "企业家",
        "ESFP" => "表演者",
        _ => "",
    }
}

// ==================== 九型人格核心 ====================

/// 一号到九号的主型加一侧翼。侧翼取主型相邻的那一个号：一号的两翼是 9 与 2，九号的
/// 两翼是 8 与 1，中间各取 ±1。这一套答的是「他为什么这样使力」，不是「他是什么型」。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Enneagram {
    #[serde(default, alias = "type", alias = "主型", alias = "core")]
    pub number: u8,
    #[serde(default, alias = "wing", alias = "侧翼")]
    pub wing: u8,
}

impl Enneagram {
    /// 主型夹回 1..9，侧翼夹回「相邻或 0」。主型不合法时整体作废（返回 `None`）。
    pub fn sanitized(self) -> Option<Self> {
        if !(1..=9).contains(&self.number) {
            return None;
        }
        let wing = if self.wing != 0 && adjacent(self.number, self.wing) {
            self.wing
        } else {
            0
        };
        Some(Self {
            number: self.number,
            wing,
        })
    }

    /// 「5w4」这样的短码，无侧翼时只给主型号。
    pub fn short(&self) -> String {
        if self.wing == 0 {
            format!("{}", self.number)
        } else {
            format!("{}w{}", self.number, self.wing)
        }
    }

    /// 主型常称，如「观察者」。
    pub fn name(&self) -> &'static str {
        ennea_name(self.number)
    }

    /// 主型一句核心理念，印在型号下当注解。
    pub fn motif(&self) -> &'static str {
        ennea_motif(self.number)
    }
}

/// 侧翼是否与主型相邻。
fn adjacent(number: u8, wing: u8) -> bool {
    (wing + 1 == number || wing == number + 1 && number < 9)
        || (number == 1 && wing == 9)
        || (number == 9 && wing == 1)
}

fn ennea_name(number: u8) -> &'static str {
    match number {
        1 => "完美主义者",
        2 => "助人者",
        3 => "成就者",
        4 => "个人主义者",
        5 => "观察者",
        6 => "忠诚者",
        7 => "活跃者",
        8 => "挑战者",
        9 => "调停者",
        _ => "",
    }
}

fn ennea_motif(number: u8) -> &'static str {
    match number {
        1 => "怕自己有错，用标准与修正握住世界",
        2 => "怕不被需要，用给予换一处安放",
        3 => "怕没有价值，把成绩当成自己的脸",
        4 => "怕平庸，在独有的情绪与品味里认自己",
        5 => "怕被吞没，退到知识与距离背后自保",
        6 => "怕失去依靠，在忠诚与质疑之间来回",
        7 => "怕被痛苦困住，用更多可能逃开当下",
        8 => "怕被掌控，用强度与直接护住自己",
        9 => "怕失联，用迁就来避开一切冲突",
        _ => "",
    }
}

// ==================== 汇成一措辞 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn mbti(e: i32, s: i32, t: i32, j: i32) -> Mbti {
        Mbti {
            energy: e,
            perceiving: s,
            deciding: t,
            lifestyle: j,
        }
    }

    #[test]
    fn mbti_code_follows_the_signs() {
        assert_eq!(mbti(-80, -60, 40, -10).code(), "INTP");
        assert_eq!(mbti(30, 60, -40, 60).code(), "ESFJ");
        // 0 归到大写那一极（正侧），最少争议。
        assert_eq!(mbti(0, 0, 0, 0).code(), "ESTJ");
    }

    #[test]
    fn mbti_epithet_reads_the_code() {
        assert_eq!(mbti(-80, -60, 40, -10).epithet(), "逻辑学家");
        assert_eq!(mbti(30, 60, -40, 60).epithet(), "执政官");
        assert_eq!(mbti(-80, -60, -70, -30).epithet(), "调停者");
    }

    #[test]
    fn axis_leans_and_poles() {
        assert_eq!(axis_lean(80), 90);
        assert_eq!(axis_lean(-30), 35);
        assert_eq!(axis_lean(0), 50);
        assert_eq!(axis_lean(200), 100);
        assert_eq!(axis_lean(-200), 0);
    }

    #[test]
    fn out_of_range_axes_are_clamped() {
        let m = mbti(999, -999, 0, 0).sanitized();
        assert_eq!(m.energy, 100);
        assert_eq!(m.perceiving, -100);
    }

    #[test]
    fn enneagram_wings_must_be_adjacent() {
        assert_eq!(Enneagram { number: 5, wing: 6 }.sanitized().unwrap().short(), "5w6");
        // 5 的邻位是 4 与 6；写成 8 就丢掉侧翼但不丢主型。
        let e = Enneagram { number: 5, wing: 8 }.sanitized().unwrap();
        assert_eq!(e.wing, 0);
        assert_eq!(e.short(), "5");
        // 一号两翼是 9 与 2，九号两翼是 8 与 1
        assert_eq!(Enneagram { number: 1, wing: 9 }.sanitized().unwrap().wing, 9);
        assert_eq!(Enneagram { number: 1, wing: 2 }.sanitized().unwrap().wing, 2);
        assert_eq!(Enneagram { number: 9, wing: 8 }.sanitized().unwrap().wing, 8);
        // 主型不合法，整体作废
        assert!(Enneagram { number: 12, wing: 0 }.sanitized().is_none());
        assert!(Enneagram { number: 0, wing: 0 }.sanitized().is_none());
    }

    #[test]
    fn enneagram_names_and_motifs_exist() {
        for n in 1..=9u8 {
            let e = Enneagram { number: n, wing: 0 }.sanitized().unwrap();
            assert!(!e.name().is_empty(), "型 {n} 缺少常称");
            assert!(!e.motif().is_empty(), "型 {n} 缺少核心理念");
        }
    }

    #[test]
    fn the_two_models_deserialize_from_a_loose_shape() {
        let raw = r#"{
            "mbti": {"EI": -75, "SN": -55, "TF": 40, "JP": -20},
            "enneagram": {"type": 5, "wing": 4}
        }"#;
        #[derive(Deserialize)]
        struct Wrapper {
            mbti: Mbti,
            enneagram: Enneagram,
        }
        let w: Wrapper = serde_json::from_str(raw).unwrap();
        assert_eq!(w.mbti.sanitized().code(), "INTP");
        assert_eq!(w.enneagram.sanitized().unwrap().short(), "5w4");
        assert_eq!(w.enneagram.sanitized().unwrap().name(), "观察者");
    }
}
