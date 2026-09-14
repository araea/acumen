//! 起卦。
//!
//! 卦不是模型编的，也不是随机抽的签——他留下的发言与数目就是蓍草。用大衍筮法
//! 真的走一遍：四十九策，分二、挂一、揲四、归奇，三变成一爻，十八变而成一卦。
//! 因此同一个人、同一批素材起出来的是同一卦；素材变了（他又说了几百句），卦才跟着变。
//!
//! 本卦之外还带变爻与之卦：老阳（九）、老阴（六）是要动的爻，把它们翻过来就是之卦。
//! 动在哪一爻，就是他现在卡在哪一处。占法按朱熹《易学启蒙》的变占例取。
//!
//! 表里存的是六十四卦的卦名、卦辞与义理。卦辞是经文原文；义理是这一卦讲的那件道理，
//! 供模型拿去落在人身上——这是全篇的骨架，不是装饰。

use super::collect::Material;
use std::fmt::Write as _;

/// 一爻。数值沿用筮法：六老阴、七少阳、八少阴、九老阳。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// 老阴，六。变爻，动而变阳。
    OldYin,
    /// 少阳，七。静爻。
    YoungYang,
    /// 少阴，八。静爻。
    YoungYin,
    /// 老阳，九。变爻，动而变阴。
    OldYang,
}

impl Line {
    /// 这一爻的筮数（六七八九）。
    pub fn value(self) -> u8 {
        match self {
            Line::OldYin => 6,
            Line::YoungYang => 7,
            Line::YoungYin => 8,
            Line::OldYang => 9,
        }
    }

    /// 是阳爻还是阴爻（看当下这一卦，不看它要变成什么）。
    pub fn yang(self) -> bool {
        matches!(self, Line::YoungYang | Line::OldYang)
    }

    /// 老阳、老阴为变爻。
    pub fn changing(self) -> bool {
        matches!(self, Line::OldYang | Line::OldYin)
    }
}

/// 六爻里的第几爻（从下往上）。名字用来写爻题：初九、六二、上六。
const POSITION: [&str; 6] = ["初", "二", "三", "四", "五", "上"];

/// 爻题：初九 / 六二 / 上六。
pub fn line_title(index: usize, yang: bool) -> String {
    let number = if yang { "九" } else { "六" };
    match index {
        0 => format!("初{number}"),
        5 => format!("上{number}"),
        other => format!("{number}{}", POSITION[other]),
    }
}

/// 爻位之义。卦各不同，爻位之理相通：初潜、二居中、三多凶、四多惧、五得位、上亢极。
pub const POSITION_SENSE: [&str; 6] = [
    "初爻在下，事刚起头，宜潜宜藏，不宜抢先",
    "二爻居下卦之中，位在臣，多得助，稳中可行",
    "三爻是下卦之极，进退之间，最易过与危",
    "四爻是上卦之初，逼近主位，多疑多惧，要看清楚上下",
    "五爻居上卦之中，是主位，居中得正，看的是担当",
    "上爻在一卦之终，事到尽头，亢则有悔，宜收不宜进",
];

/// 一卦。
pub struct Hexagram {
    /// 序卦传里的次第，一到六十四。
    pub number: u8,
    /// 卦名，如「屯」。
    pub name: &'static str,
    /// 全名，如「水雷屯」。
    pub full: &'static str,
    /// 六爻的阴阳，bit0 是初爻，1 为阳。
    pub binary: u8,
    /// 卦辞，经文原文。
    pub judgment: &'static str,
    /// 这一卦讲的道理。
    pub sense: &'static str,
}

impl Hexagram {
    /// 上卦（外卦）的卦画。
    fn upper(&self) -> u8 {
        (self.binary >> 3) & 0b111
    }

    /// 下卦（内卦）的卦画。
    fn lower(&self) -> u8 {
        self.binary & 0b111
    }

    /// 「上坎下震」这种说法。
    pub fn trigrams(&self) -> String {
        format!("上{}下{}", trigram_name(self.upper()), trigram_name(self.lower()))
    }
}

/// 八卦：卦画（bit0 在下）→ 卦名。
fn trigram_name(bits: u8) -> &'static str {
    match bits & 0b111 {
        0b111 => "乾",
        0b110 => "兑",
        0b101 => "离",
        0b100 => "震",
        0b011 => "巽",
        0b010 => "坎",
        0b001 => "艮",
        _ => "坤",
    }
}

/// 六十四卦，按序卦传的次序。卦辞为经文原文，义理是这一卦的道理。
pub static HEXAGRAMS: [Hexagram; 64] = [
    Hexagram { number: 1, name: "乾", full: "乾为天", binary: 0b111111,
        judgment: "元亨利贞。",
        sense: "纯阳至健，是力量最足的一卦。自强的道理：势越盛，越怕盈满。" },
    Hexagram { number: 2, name: "坤", full: "坤为地", binary: 0b000000,
        judgment: "元亨，利牝马之贞。君子有攸往，先迷后得主，利西南得朋，东北丧朋。安贞吉。",
        sense: "纯阴至顺，是承载与跟随的位置。不争先，先迷后得，靠的是守静与借力。" },
    Hexagram { number: 3, name: "屯", full: "水雷屯", binary: 0b010100,
        judgment: "元亨利贞，勿用有攸往，利建侯。",
        sense: "初生就遇上险，万事开头难。这时候不宜远行，宜扎住根、找对人。" },
    Hexagram { number: 4, name: "蒙", full: "山水蒙", binary: 0b001010,
        judgment: "亨。匪我求童蒙，童蒙求我。初筮告，再三渎，渎则不告。利贞。",
        sense: "蒙昧未开，是要学要问的时候。问一次就够，反复试探反而不灵。" },
    Hexagram { number: 5, name: "需", full: "水天需", binary: 0b010111,
        judgment: "有孚，光亨，贞吉。利涉大川。",
        sense: "云在天上，雨还没落。该等的就等——等不是干耗，是把自己备好。" },
    Hexagram { number: 6, name: "讼", full: "天水讼", binary: 0b111010,
        judgment: "有孚窒惕，中吉，终凶。利见大人，不利涉大川。",
        sense: "争。赢到底也算输，中途收手才是上策。" },
    Hexagram { number: 7, name: "师", full: "地水师", binary: 0b000010,
        judgment: "贞，丈人吉，无咎。",
        sense: "聚众。要有规矩，要有主事的人，名正才言顺。" },
    Hexagram { number: 8, name: "比", full: "水地比", binary: 0b010000,
        judgment: "吉。原筮，元永贞，无咎。不宁方来，后夫凶。",
        sense: "亲附。跟对人比什么都强，早跟比晚跟好。" },
    Hexagram { number: 9, name: "小畜", full: "风天小畜", binary: 0b011111,
        judgment: "亨。密云不雨，自我西郊。",
        sense: "小有积蓄，力还不够。密云不雨，先攒着，别急着要结果。" },
    Hexagram { number: 10, name: "履", full: "天泽履", binary: 0b111110,
        judgment: "履虎尾，不咥人，亨。",
        sense: "踩着虎尾走路。分寸就是性命，谨慎与礼数是护身的东西。" },
    Hexagram { number: 11, name: "泰", full: "地天泰", binary: 0b000111,
        judgment: "小往大来，吉亨。",
        sense: "通。上下相交，气脉顺。顺境里要防的是把通当成当然。" },
    Hexagram { number: 12, name: "否", full: "天地否", binary: 0b111000,
        judgment: "否之匪人，不利君子贞，大往小来。",
        sense: "塞。上下不交，说什么也不通。此时守住自己，不必硬推。" },
    Hexagram { number: 13, name: "同人", full: "天火同人", binary: 0b111101,
        judgment: "同人于野，亨。利涉大川，利君子贞。",
        sense: "与人和同。在开阔处结伴，不在门户里结党。" },
    Hexagram { number: 14, name: "大有", full: "火天大有", binary: 0b101111,
        judgment: "元亨。",
        sense: "所有甚丰。手里有东西的时候，考验的不是本事，是怎么拿。" },
    Hexagram { number: 15, name: "谦", full: "地山谦", binary: 0b000001,
        judgment: "亨，君子有终。",
        sense: "谦。有实而能下，六爻里只有这一卦没有凶辞。" },
    Hexagram { number: 16, name: "豫", full: "雷地豫", binary: 0b100000,
        judgment: "利建侯行师。",
        sense: "豫乐，也是预备。顺而动，乐要有节制，乐之前要先有准备。" },
    Hexagram { number: 17, name: "随", full: "泽雷随", binary: 0b110100,
        judgment: "元亨利贞，无咎。",
        sense: "随顺时势。跟着走没错，要紧的是知道自己跟的是什么。" },
    Hexagram { number: 18, name: "蛊", full: "山风蛊", binary: 0b001011,
        judgment: "元亨，利涉大川。先甲三日，后甲三日。",
        sense: "积弊成蛊。坏掉的东西要治，治之前先把怎么坏的弄清楚。" },
    Hexagram { number: 19, name: "临", full: "地泽临", binary: 0b000110,
        judgment: "元亨利贞。至于八月有凶。",
        sense: "居高临下。势好的时候，要想到八月。" },
    Hexagram { number: 20, name: "观", full: "风地观", binary: 0b011000,
        judgment: "盥而不荐，有孚颙若。",
        sense: "观。既在看别人，也在被别人看。诚敬比排场管用。" },
    Hexagram { number: 21, name: "噬嗑", full: "火雷噬嗑", binary: 0b101100,
        judgment: "亨。利用狱。",
        sense: "咬合。中间卡着硬东西，非咬碎不能通——该断的就得断。" },
    Hexagram { number: 22, name: "贲", full: "山火贲", binary: 0b001101,
        judgment: "亨。小利有攸往。",
        sense: "文饰。修饰只宜小用，本色才要紧。" },
    Hexagram { number: 23, name: "剥", full: "山地剥", binary: 0b001000,
        judgment: "不利有攸往。",
        sense: "剥落。根基在一层层被削，此时宜止不宜进。" },
    Hexagram { number: 24, name: "复", full: "地雷复", binary: 0b000100,
        judgment: "亨。出入无疾，朋来无咎。反复其道，七日来复，利有攸往。",
        sense: "一阳来复。回头就是路，重新开始不丢人。" },
    Hexagram { number: 25, name: "无妄", full: "天雷无妄", binary: 0b111100,
        judgment: "元亨利贞。其匪正有眚，不利有攸往。",
        sense: "不妄。按本分做事就顺，动了歪念就有灾。" },
    Hexagram { number: 26, name: "大畜", full: "山天大畜", binary: 0b001111,
        judgment: "利贞，不家食吉，利涉大川。",
        sense: "大积蓄。德与力都攒够了才谈得出山。" },
    Hexagram { number: 27, name: "颐", full: "山雷颐", binary: 0b001100,
        judgment: "贞吉。观颐，自求口实。",
        sense: "养。看一个人拿什么养自己、养谁，就知道他是什么人。" },
    Hexagram { number: 28, name: "大过", full: "泽风大过", binary: 0b110011,
        judgment: "栋桡，利有攸往，亨。",
        sense: "过甚。梁都要压弯了，非常的时候只能用非常的分量。" },
    Hexagram { number: 29, name: "坎", full: "坎为水", binary: 0b010010,
        judgment: "习坎，有孚，维心亨，行有尚。",
        sense: "重重险。险里能靠的只有一颗定住的心。" },
    Hexagram { number: 30, name: "离", full: "离为火", binary: 0b101101,
        judgment: "利贞，亨。畜牝牛，吉。",
        sense: "附丽。火要附着在东西上才亮，人要有可依之处。" },
    Hexagram { number: 31, name: "咸", full: "泽山咸", binary: 0b110001,
        judgment: "亨，利贞，取女吉。",
        sense: "感应。无心之感最真，感应是从虚而受来的。" },
    Hexagram { number: 32, name: "恒", full: "雷风恒", binary: 0b100011,
        judgment: "亨，无咎，利贞，利有攸往。",
        sense: "恒久。立得住是因为守常，不是因为猛。" },
    Hexagram { number: 33, name: "遁", full: "天山遁", binary: 0b111001,
        judgment: "亨，小利贞。",
        sense: "退避。该退的时候退，退不是败。" },
    Hexagram { number: 34, name: "大壮", full: "雷天大壮", binary: 0b100111,
        judgment: "利贞。",
        sense: "壮盛。力气大的时候最容易用错，壮而守正是全部。" },
    Hexagram { number: 35, name: "晋", full: "火地晋", binary: 0b101000,
        judgment: "康侯用锡马蕃庶，昼日三接。",
        sense: "进。光明在上了，是向上走的时候。" },
    Hexagram { number: 36, name: "明夷", full: "地火明夷", binary: 0b000101,
        judgment: "利艰贞。",
        sense: "明入地中。光被压住了，此时把光收起来，藏明于晦。" },
    Hexagram { number: 37, name: "家人", full: "风火家人", binary: 0b011101,
        judgment: "利女贞。",
        sense: "内。一个地方的风气，是从里面先立起来的。" },
    Hexagram { number: 38, name: "睽", full: "火泽睽", binary: 0b101110,
        judgment: "小事吉。",
        sense: "乖离。同中有异，容得下异才走得下去。" },
    Hexagram { number: 39, name: "蹇", full: "水山蹇", binary: 0b010001,
        judgment: "利西南，不利东北。利见大人，贞吉。",
        sense: "跛行难进。难就绕，别拿头去撞。" },
    Hexagram { number: 40, name: "解", full: "雷水解", binary: 0b100010,
        judgment: "利西南，无所往，其来复吉。有攸往，夙吉。",
        sense: "解冻。该散的散了，动作要早。" },
    Hexagram { number: 41, name: "损", full: "山泽损", binary: 0b001110,
        judgment: "有孚，元吉，无咎，可贞，利有攸往。曷之用？二簋可用享。",
        sense: "减损。少而诚，比多而虚有用。" },
    Hexagram { number: 42, name: "益", full: "风雷益", binary: 0b011100,
        judgment: "利有攸往，利涉大川。",
        sense: "增益。损上益下，越给越多。" },
    Hexagram { number: 43, name: "夬", full: "泽天夬", binary: 0b110111,
        judgment: "扬于王庭，孚号有厉。告自邑，不利即戎，利有攸往。",
        sense: "决断。该断的要断，但要在明处断，不靠蛮力。" },
    Hexagram { number: 44, name: "姤", full: "天风姤", binary: 0b111011,
        judgment: "女壮，勿用取女。",
        sense: "不期而遇。一阴初生，遇上得太容易的东西要留神。" },
    Hexagram { number: 45, name: "萃", full: "泽地萃", binary: 0b110000,
        judgment: "亨。王假有庙，利见大人，亨，利贞。用大牲吉，利有攸往。",
        sense: "聚。人聚起来，要有由头，也要有主心骨。" },
    Hexagram { number: 46, name: "升", full: "地风升", binary: 0b000011,
        judgment: "元亨，用见大人，勿恤，南征吉。",
        sense: "上升。像树一样长，不急，积小成高。" },
    Hexagram { number: 47, name: "困", full: "泽水困", binary: 0b110010,
        judgment: "亨，贞，大人吉，无咎。有言不信。",
        sense: "困。说了也没人信的时候，就少说，守住里面的那点通。" },
    Hexagram { number: 48, name: "井", full: "水风井", binary: 0b010011,
        judgment: "改邑不改井，无丧无得，往来井井。汔至亦未繘井，羸其瓶，凶。",
        sense: "井。滋养人的本事在井底，打不上来就等于没有。" },
    Hexagram { number: 49, name: "革", full: "泽火革", binary: 0b110101,
        judgment: "己日乃孚，元亨利贞，悔亡。",
        sense: "变革。改要等到时候，改得对才没有悔。" },
    Hexagram { number: 50, name: "鼎", full: "火风鼎", binary: 0b101011,
        judgment: "元吉，亨。",
        sense: "鼎新。把东西重新煮过一遍，换一副骨头。" },
    Hexagram { number: 51, name: "震", full: "震为雷", binary: 0b100100,
        judgment: "亨。震来虩虩，笑言哑哑。震惊百里，不丧匕鬯。",
        sense: "动。雷来了先怕后笑，动里不失自己的分寸。" },
    Hexagram { number: 52, name: "艮", full: "艮为山", binary: 0b001001,
        judgment: "艮其背，不获其身，行其庭，不见其人，无咎。",
        sense: "止。该停就停住。止住自己比止住别人难。" },
    Hexagram { number: 53, name: "渐", full: "风山渐", binary: 0b011001,
        judgment: "女归吉，利贞。",
        sense: "渐进。按次序来，慢就是快。" },
    Hexagram { number: 54, name: "归妹", full: "雷泽归妹", binary: 0b100110,
        judgment: "征凶，无攸利。",
        sense: "失序而合。名分不正的关系，再往前就是凶。" },
    Hexagram { number: 55, name: "丰", full: "雷火丰", binary: 0b100101,
        judgment: "亨，王假之，勿忧，宜日中。",
        sense: "丰大。像正午一样亮，但要记得正午之后就是斜阳。" },
    Hexagram { number: 56, name: "旅", full: "火山旅", binary: 0b101001,
        judgment: "小亨，旅贞吉。",
        sense: "旅。人在外头，谦下就顺。" },
    Hexagram { number: 57, name: "巽", full: "巽为风", binary: 0b011011,
        judgment: "小亨，利有攸往，利见大人。",
        sense: "入。风无孔不入，柔而能进，重复比用力有效。" },
    Hexagram { number: 58, name: "兑", full: "兑为泽", binary: 0b110110,
        judgment: "亨，利贞。",
        sense: "悦。说服人、被人喜欢，都要走正道。" },
    Hexagram { number: 59, name: "涣", full: "风水涣", binary: 0b011010,
        judgment: "亨。王假有庙，利涉大川，利贞。",
        sense: "涣散。散得开才聚得拢，先把隔阂化开。" },
    Hexagram { number: 60, name: "节", full: "水泽节", binary: 0b010110,
        judgment: "亨。苦节不可贞。",
        sense: "节制。有度才通；苦着自己去节，守不长。" },
    Hexagram { number: 61, name: "中孚", full: "风泽中孚", binary: 0b011110,
        judgment: "豚鱼吉，利涉大川，利贞。",
        sense: "信在中。诚到连豚鱼都能感，才涉得过大川。" },
    Hexagram { number: 62, name: "小过", full: "雷山小过", binary: 0b100001,
        judgment: "亨，利贞。可小事，不可大事。飞鸟遗之音，不宜上，宜下，大吉。",
        sense: "小有过越。小事上稍微过一点无妨，大事上不行；宜下不宜上。" },
    Hexagram { number: 63, name: "既济", full: "水火既济", binary: 0b010101,
        judgment: "亨，小利贞，初吉终乱。",
        sense: "已成。做成了，反而要防乱——初吉终乱。" },
    Hexagram { number: 64, name: "未济", full: "火水未济", binary: 0b101010,
        judgment: "亨，小狐汔济，濡其尾，无攸利。",
        sense: "未成。还没过河，小狐湿了尾巴，收尾要审慎。" },
];

/// 按六爻卦画取卦。表覆盖了全部六十四种组合。
pub fn hexagram(binary: u8) -> &'static Hexagram {
    HEXAGRAMS
        .iter()
        .find(|hex| hex.binary == (binary & 0b111111))
        .expect("六十四卦表覆盖全部六爻组合")
}

/// 一次起卦的所得。
pub struct Cast {
    /// 六爻，下标 0 是初爻。
    pub lines: [Line; 6],
    /// 每一爻三变之后余下的策数：三十六、三十二、二十八、二十四。
    pub stalks: [u8; 6],
    /// 本卦。
    pub primary: &'static Hexagram,
    /// 之卦。六爻皆静时为 `None`。
    pub changed: Option<&'static Hexagram>,
    /// 变爻的爻位，下标 0 是初爻。
    pub changing: Vec<usize>,
}

impl Cast {
    /// 变占法，按朱熹《易学启蒙》的变占例。
    pub fn rule(&self) -> &'static str {
        match self.changing.len() {
            0 => "六爻皆静，以本卦卦辞为占",
            1 => "一爻变，以本卦之变爻为占",
            2 => "二爻变，以本卦两条变爻为占，上爻为主",
            3 => "三爻变，本卦与之卦并看，以本卦为主",
            4 => "四爻变，以之卦两条不变爻为占，下爻为主",
            5 => "五爻变，以之卦之不变爻为占",
            _ => "六爻皆变，以之卦卦辞为占",
        }
    }

    /// 三变所得的策数，初爻到上爻：「36 · 32 · 28 · 24 · 36 · 32」。
    pub fn stalks_text(&self) -> String {
        let mut out = String::new();
        for (index, value) in self.stalks.iter().enumerate() {
            if index > 0 {
                out.push_str(" · ");
            }
            let _ = write!(out, "{value}");
        }
        out
    }

    /// 变爻的爻题，如「九三」。
    pub fn changing_titles(&self) -> Vec<String> {
        self.changing
            .iter()
            .map(|index| line_title(*index, self.lines[*index].yang()))
            .collect()
    }

    /// 六爻阴阳，初爻到上爻。变爻以「动」标出。
    pub fn drawn(&self) -> [DrawnLine; 6] {
        let mut out = [DrawnLine::default(); 6];
        for (index, slot) in out.iter_mut().enumerate() {
            let line = self.lines[index];
            *slot = DrawnLine {
                yang: line.yang(),
                changing: line.changing(),
            };
        }
        out
    }
}

/// 画爻用的一行：阴阳，以及是否在动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrawnLine {
    pub yang: bool,
    pub changing: bool,
}

/// 起卦。种子取自这个人留下的素材，同一批素材必然得同一卦。
pub fn cast(material: &Material) -> Cast {
    cast_from(seed_of(material))
}

/// 由种子起卦。分出这一步是为了让测试能钉住确定性与取值范围。
pub fn cast_from(seed: u64) -> Cast {
    let mut rng = SplitMix64::new(seed);
    let mut lines = [Line::YoungYang; 6];
    let mut stalks = [0u8; 6];
    for index in 0..6 {
        let (line, remaining) = one_line(&mut rng);
        lines[index] = line;
        stalks[index] = remaining;
    }

    let mut binary = 0u8;
    let mut changing = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.yang() {
            binary |= 1 << index;
        }
        if line.changing() {
            changing.push(index);
        }
    }
    // 之卦：变爻翻过来，静爻照旧。
    let mut changed_binary = binary;
    for index in &changing {
        changed_binary ^= 1 << index;
    }
    let primary = hexagram(binary);
    let changed = (!changing.is_empty()).then(|| hexagram(changed_binary));

    Cast {
        lines,
        stalks,
        primary,
        changed,
        changing,
    }
}

/// 三变成一爻：四十九策，分二、挂一、揲四、归奇，三遍之后剩三十六、三十二、
/// 二十八或二十四策，除以四得九、八、七、六。
fn one_line(rng: &mut SplitMix64) -> (Line, u8) {
    let mut remaining: u32 = 49;
    for _ in 0..3 {
        // 分二：随手把这一堆分成左右两份，左堆至少一根，右堆至少一根。
        let left = 1 + rng.below(remaining as u64 - 1) as u32;
        // 挂一：从右堆里取一根，挂在指间。
        let right = remaining - left - 1;
        remaining -= remainder(left) + remainder(right) + 1; // 归奇
    }
    let line = match remaining {
        36 => Line::OldYang,
        32 => Line::YoungYin,
        28 => Line::YoungYang,
        24 => Line::OldYin,
        other => unreachable!("三变之后只可能是 36/32/28/24，得到 {other}"),
    };
    (line, remaining as u8)
}

/// 揲四之后的余数。整除时按传统记作四，不记零。
fn remainder(pile: u32) -> u32 {
    match pile % 4 {
        0 => 4,
        other => other,
    }
}

/// 素材的指纹，作起卦的根。
///
/// 取的是「这个人是谁」：发言的数目、起止、跨度，以及他真说过的那些句子。
/// 用 FNV-1a，只求稳（同一份素材每次同值），不求密码学上的强度。
pub fn seed_of(material: &Material) -> u64 {
    let mut fnv = Fnv::new();
    fnv.number(material.user_id as u64);
    fnv.number(material.total);
    fnv.number(material.first_time as u64);
    fnv.number(material.last_time as u64);
    fnv.number(material.active_days);
    fnv.number(material.longest);
    fnv.number(material.avg_len.to_bits());
    for sample in &material.samples {
        fnv.eat(sample.as_bytes());
        fnv.eat(b"\x1f");
    }
    for (word, count) in &material.words {
        fnv.eat(word.as_bytes());
        fnv.number(*count);
    }
    fnv.0
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn eat(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn number(&mut self, value: u64) {
        self.eat(&value.to_le_bytes());
    }
}

/// splitmix64。起卦只要一个确定的、分布够匀的伪随机源，不引额外的依赖。
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `0..bound` 上取值，`bound` 必须大于零。
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::portrait::collect::{GroupSlice, Kinds};

    fn material() -> Material {
        Material {
            user_id: 3373167460,
            name: "甲".into(),
            total: 400,
            first_time: 1_700_000_000,
            last_time: 1_700_000_000 + 86_400 * 29,
            active_days: 20,
            hour: [0; 24],
            weekday: [0; 7],
            groups: vec![GroupSlice {
                name: "测试群".into(),
                count: 300,
            }],
            kinds: Kinds::default(),
            longest: 210,
            avg_len: 18.0,
            words: vec![("天气".into(), 12)],
            samples: vec!["今天这个雨下得没完没了".to_string(), "凌晨三点还在改".to_string()],
        }
    }

    /// 表必须正好盖住六十四种卦画，一种不漏、一种不重——否则取卦会撞上断言。
    #[test]
    fn the_table_covers_every_combination_exactly_once() {
        assert_eq!(HEXAGRAMS.len(), 64);
        let mut seen = [false; 64];
        for hex in &HEXAGRAMS {
            let slot = &mut seen[hex.binary as usize];
            assert!(!*slot, "卦画 {:#08b} 重复：{}", hex.binary, hex.name);
            *slot = true;
            assert_eq!(hex.binary >> 6, 0, "卦画只该有六位：{}", hex.full);
            assert!((1..=64).contains(&hex.number));
        }
        assert!(seen.iter().all(|hit| *hit), "有卦画没被覆盖");
        // 序卦传的次第也应当不重不漏。
        let mut numbers: Vec<u8> = HEXAGRAMS.iter().map(|hex| hex.number).collect();
        numbers.sort_unstable();
        assert_eq!(numbers, (1..=64).collect::<Vec<u8>>());
    }

    /// 几卦的对读：卦画、卦名、上下卦三处必须自洽。
    #[test]
    fn a_few_hexagrams_read_as_they_should() {
        let qian = hexagram(0b111111);
        assert_eq!(qian.full, "乾为天");
        assert_eq!(qian.judgment, "元亨利贞。");
        assert_eq!(qian.trigrams(), "上乾下乾");

        let zhun = hexagram(0b010100);
        assert_eq!(zhun.name, "屯");
        assert_eq!(zhun.number, 3);
        assert_eq!(zhun.trigrams(), "上坎下震");
        assert_eq!(zhun.upper(), 0b010);
        assert_eq!(zhun.lower(), 0b100);

        let weiji = hexagram(0b101010);
        assert_eq!(weiji.full, "火水未济");
        assert_eq!(weiji.number, 64);
        assert_eq!(weiji.trigrams(), "上离下坎");
    }

    /// 表是手抄的，最容易错的就是卦画。八卦的象就写在八个纯卦的名字里
    /// （乾为天、坎为水……），拿它们当尺子量另外五十六卦：全名必须等于
    /// 「上卦之象 + 下卦之象 + 卦名」。这一条能钉住整张表。
    #[test]
    fn every_name_matches_its_two_trigrams() {
        let mut images = std::collections::HashMap::new();
        for hex in &HEXAGRAMS {
            let (lower, upper) = (hex.binary & 0b111, (hex.binary >> 3) & 0b111);
            if lower != upper {
                continue;
            }
            let (name, image) = hex
                .full
                .split_once('为')
                .unwrap_or_else(|| panic!("纯卦应当写作「某为某」：{}", hex.full));
            assert_eq!(name, hex.name, "{} 的卦名对不上", hex.full);
            let previous = images.insert(lower, image);
            assert!(previous.is_none(), "下卦 {lower:#05b} 出现了两次");
        }
        assert_eq!(images.len(), 8, "八个纯卦应当把八卦的象都定下来");

        for hex in &HEXAGRAMS {
            let lower = hex.binary & 0b111;
            let upper = (hex.binary >> 3) & 0b111;
            if lower == upper {
                continue;
            }
            let expected = format!(
                "{}{}{}",
                images[&upper], images[&lower], hex.name
            );
            assert_eq!(hex.full, expected, "第 {} 卦的卦画与全名不符", hex.number);
        }
    }

    /// 筮法的取值只可能是六七八九；策数只可能是三十六到二十四那四档。
    #[test]
    fn the_stalks_only_ever_land_on_four_values() {
        for seed in 0..2_000u64 {
            let cast = cast_from(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            for (index, line) in cast.lines.iter().enumerate() {
                assert!(
                    (6..=9).contains(&line.value()),
                    "第 {} 爻得到 {}",
                    index,
                    line.value()
                );
                assert!(matches!(cast.stalks[index], 24 | 28 | 32 | 36));
                // 策数除以四就是这一爻的数。
                assert_eq!(cast.stalks[index] / 4, line.value());
            }
        }
    }

    /// 同一份素材起出来的是同一卦；素材动了卦才动。
    #[test]
    fn the_same_material_casts_the_same_hexagram() {
        let one = material();
        let again = cast(&one);
        let repeat = cast(&one);
        assert_eq!(again.primary.binary, repeat.primary.binary);
        assert_eq!(again.stalks, repeat.stalks);

        let mut other = material();
        other.samples.push("又说了新的一句".to_string());
        let moved = cast(&other);
        // 卦有可能不变，但种子必然不同；这里只钉住种子确实跟着素材走。
        assert_ne!(seed_of(&one), seed_of(&other));
        let _ = moved;
    }

    /// 变爻翻过来就是之卦；没有变爻时没有之卦。
    #[test]
    fn changing_lines_flip_into_the_changed_hexagram() {
        let mut saw_changing = false;
        let mut saw_still = false;
        for seed in 0..500u64 {
            let cast = cast_from(seed);
            if cast.changing.is_empty() {
                saw_still = true;
                assert!(cast.changed.is_none(), "没有变爻就不该有之卦");
                assert_eq!(cast.rule(), "六爻皆静，以本卦卦辞为占");
                continue;
            }
            saw_changing = true;
            let changed = cast.changed.expect("有变爻就该有之卦");
            for (index, line) in cast.lines.iter().enumerate() {
                let before = (cast.primary.binary >> index) & 1 == 1;
                let after = (changed.binary >> index) & 1 == 1;
                if line.changing() {
                    assert_ne!(before, after, "变爻第 {} 位没有翻过来", index);
                    assert!(cast.changing.contains(&index));
                } else {
                    assert_eq!(before, after, "静爻第 {} 位不该动", index);
                }
            }
            assert_eq!(cast.changing_titles().len(), cast.changing.len());
        }
        assert!(saw_changing && saw_still, "五百次里应当有静卦也有变卦");
    }

    /// 变占法随变爻条数走，六种都要能对上。
    #[test]
    fn the_divination_rule_follows_the_count_of_changing_lines() {
        let titles: Vec<String> = (0..6).map(|index| line_title(index, index % 2 == 0)).collect();
        assert_eq!(
            titles,
            vec!["初九", "六二", "九三", "六四", "九五", "上六"]
        );

        let cases = [
            (0, "六爻皆静，以本卦卦辞为占"),
            (1, "一爻变，以本卦之变爻为占"),
            (2, "二爻变，以本卦两条变爻为占，上爻为主"),
            (3, "三爻变，本卦与之卦并看，以本卦为主"),
            (4, "四爻变，以之卦两条不变爻为占，下爻为主"),
            (5, "五爻变，以之卦之不变爻为占"),
            (6, "六爻皆变，以之卦卦辞为占"),
        ];
        for (count, expected) in cases {
            let mut lines = [Line::YoungYang; 6];
            let mut changing = Vec::new();
            for (index, slot) in lines.iter_mut().enumerate().take(count) {
                *slot = Line::OldYang;
                changing.push(index);
            }
            let cast = Cast {
                lines,
                stalks: [36, 32, 28, 24, 36, 32],
                primary: hexagram(0b111111),
                changed: (!changing.is_empty()).then(|| hexagram(0b000000)),
                changing,
            };
            assert_eq!(cast.rule(), expected, "{count} 条变爻");
        }
    }

    #[test]
    fn the_stalk_line_reads_from_the_first_line_up() {
        let cast = Cast {
            lines: [Line::OldYang; 6],
            stalks: [36, 32, 28, 24, 36, 32],
            primary: hexagram(0b111111),
            changed: Some(hexagram(0b000000)),
            changing: (0..6).collect(),
        };
        assert_eq!(cast.stalks_text(), "36 · 32 · 28 · 24 · 36 · 32");
        assert_eq!(cast.changing_titles().len(), 6);
    }

    /// 抽到的卦要铺得开，不能总落在同一卦上。
    #[test]
    fn casts_are_spread_over_the_sixy_four() {
        let mut seen = std::collections::HashSet::new();
        for seed in 0..4_000u64 {
            seen.insert(cast_from(seed).primary.number);
        }
        assert!(seen.len() > 50, "四千次只起了 {} 卦", seen.len());
    }

    /// 画爻的行数与变爻标记都对得上。
    #[test]
    fn the_drawn_lines_carry_yin_yang_and_movement() {
        let cast = cast(&material());
        let drawn = cast.drawn();
        assert_eq!(drawn.len(), 6);
        for (index, line) in drawn.iter().enumerate() {
            assert_eq!(line.yang, cast.lines[index].yang());
            assert_eq!(line.changing, cast.lines[index].changing());
        }
    }
}
