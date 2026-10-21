# Third-party notices

OAI harness 的设计改编自 MIT 许可的 [oh-my-pi](https://github.com/can1357/oh-my-pi)：
工具循环约定，以及免密钥 public web 搜索的多后端顺序回退，其中包含它的 DuckDuckGo
结果解析器。

控制台的图标是 Apache-2.0 许可的 [Material Symbols](https://github.com/marella/material-symbols)
（Google，Rounded 字形、字重 400）：`scripts/make-icons.py` 从 `@material-symbols/svg-400` 取出用到的
几十个字形，内联在 `res/console/index.html` 的精灵图里，字形本身没有改动。

控制台「外观」的配色引擎 `res/console/theme.js` 打包了 Apache-2.0 许可的
[Material Color Utilities](https://github.com/material-foundation/material-color-utilities) 0.4.0（Google），
由 `scripts/make-theme.sh` 用 esbuild 构建，源码未改动。

奶龙提醒的识图模型 [MobileCLIP-S0](https://huggingface.co/apple/MobileCLIP-S0)（Apple，ONNX 版由
[Xenova](https://huggingface.co/Xenova/mobileclip_s0) 转换）**不随本软件分发**：插件第一次用到时从 Hugging Face
下载到本机，使用受 Apple 的 Machine Learning Research Model 许可证约束。编进可执行文件的分类头
（`src/plugins/nailong/head.bin`）是本仓库在下列公开图片集上训练的权重，不含图片本身：
[XiC1/nailong-dataset](https://huggingface.co/datasets/XiC1/nailong-dataset)、
[Logxk/nailong](https://github.com/Logxk/nailong)、[whxoperator/nailongnaiwa](https://github.com/whxoperator/nailongnaiwa)、
[Nailong-Studio/wallpaper](https://github.com/Nailong-Studio/wallpaper)、[GeorgeNaNN/nailong-memes](https://github.com/GeorgeNaNN/nailong-memes)。

## Material Symbols、Material Color Utilities

Copyright Google LLC. Licensed under the Apache License, Version 2.0
（<https://www.apache.org/licenses/LICENSE-2.0>）；按该许可，本软件按「原样」提供，不附带任何明示或暗示的担保。

## oh-my-pi

MIT License

Copyright (c) 2025 Mario Zechner

Copyright (c) 2025-2026 Can Bölük

Copyright (c) 2026 Stencil Labs, Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
