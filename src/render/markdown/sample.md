---
title: 阅读体验样张
author: 知微
date: 2026-09-29
---

## 概览

这是一段**正文**，混排 *English words*、`inline code`、~~作废内容~~ 与一个[带说明的链接](https://m3.material.io/styles/typography/overview)。中文长句要有舒适的行距与断行，价格 10~20 元也不该被划掉。第二行
紧接着写，聊天里的换行应当原样保留。

### 要点

- 第一项，配上一句稍长的说明文字，检查换行后的悬挂缩进是否整齐
- 第二项
  - 嵌套的子项
  - 另一个子项
    - 再深一层
1. 有序列表
2. 第二条
3. 第三条

- [x] 已完成的任务
- [ ] 待办的任务，说明文字长一点以便检查换行时复选框是否仍然对齐

> 一段普通引用，用来放别人说过的话。
>
> 第二段。

> [!NOTE]
> 说明：补充背景信息。

> [!TIP]
> 提示：更好的做法，含 `code` 与[链接](https://www.w3.org/TR/WCAG22/)。

> [!IMPORTANT]
> 重要：关键的前提条件。

> [!WARNING]
> 注意：可能出错的地方。

> [!CAUTION]
> 小心：不可逆的操作。

## 代码

```rust
use std::collections::HashMap;

/// 统计词频。
fn count<'a>(text: &'a str) -> HashMap<&'a str, usize> {
    let mut map = HashMap::new();
    for word in text.split_whitespace() {
        *map.entry(word).or_insert(0) += 1; // 计数
    }
    println!("共 {} 个词", map.len());
    map
}
```

```python
def greet(name: str = "世界") -> str:
    """打招呼。"""
    return f"你好，{name}！" if name else None
```

```json
{ "name": "acumen", "version": 3, "ok": true, "tags": ["a", "b"] }
```

```diff
@@ -1,3 +1,3 @@
 unchanged
-旧的一行
+新的一行
```

```bash
# 构建并重启
cargo build --release --locked && ./bot restart
```

## 表格

| 插件 | 状态 | 说明 |
|:--|:-:|--:|
| 词云 | 已启用 | 按消息记录生成 |
| 统计 | 已启用 | 排行榜与走势 |
| 网页截图 | 已停用 | 需要浏览器 |

## 其他

术语
: 定义列表里的解释文字。

行内公式 $E=mc^2$ 与块级公式：

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

![架构图](https://example.com/a.png)

脚注引用[^a]与另一个链接[规范](https://www.w3.org/TR/WCAG22/)。

---

<div>原始 HTML 只当文字显示</div>

[^a]: 这是脚注的内容。
