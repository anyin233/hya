# Research: hya 图标的终端绘制方案

- **问题**：为 hya 的启动页图标选择高性能、高精度且可回退的终端绘制方案。
- **范围**：Kitty Graphics Protocol、iTerm2 inline images、SIXEL、Unicode
  Braille/block/cell 绘制，以及可用的现代补充方案；包含性能、缩放、更新/动画、
  支持与探测、回退和打包影响。
- **日期**：2026-07-23。
- **证据标准**：协议/终端行为只使用规范、终端作者文档、Unicode 标准和当前安装的
  OpenTUI 声明文件。标为“推断”的内容是针对 hya 的工程判断，不是性能基准结果。

## 结论

**默认保留现有 Unicode quadrant-block 图标；若未来需要在较大尺寸保持原始 PNG 的
像素级外观，再新增一个严格探测、可关闭的 Kitty 后端。**

这不是因为 Kitty 不够强，而是因为 hya 当前图标很小、静态、处于一个由 OpenTUI
持续重绘的交互界面中。现有文本图形无需私有控制序列、无需额外运行时资源、可由
现有文本 diff 直接管理。Kitty 是唯一一个在本次比较中同时明确提供像素位置、可缩放
placement、图像/placement ID、无闪烁替换和终端侧增量动画的协议，因而应是“高保真
可选层”的首选，而不是所有用户的默认路径。该优先级是**工程推断**，不是跨终端的
性能跑分。

不建议把 iTerm2 或 SIXEL 作为首版的必经路径：前者是很好的 macOS/iTerm2/WezTerm
兼容分支，但文档化模型是 `OSC 1337;File` 传输而不是 placement 场景图；后者可提供
像素位图，却需要单独的 palette/SIXEL 编码与更多终端兼容性处理。它们都应在有明确
受众需求时再加入。

## 已有基线：24 × 8 个 cell，48 × 16 个逻辑子像素

当前 `LOGO_ART` 是 **24 × 8 个文本 cell**。每个 cell 是一个 Unicode quadrant/block
glyph，编码 **2 × 2** 个逻辑子像素，所以轮廓的栅格上限是 **48 × 16 子像素**，不是
显示器物理像素。[数据文件](../../packages/hya-tui-ts/src/upstream/component/logo-art.data.ts)
和[生成器](../../packages/hya-tui-ts/scripts/generate-logo-art.py)明确声明了 2 × 2
映射、透明空格和单色输出；Unicode 也正式定义了这些 quadrant terminal graphics
characters，例如 `▖`、`▗`、`▘`、`▝`。 [Unicode Block Elements](https://www.unicode.org/charts/nameslist/n_2580.html)

| 维度 | 已验证事实 | 对 hya 的含义（推断） |
| --- | --- | --- |
| 分辨率 | 24 × 8 cell × 每 cell 2 × 2 子像素 = 48 × 16 逻辑采样点。生成器先将 logo alpha 下采样，再阈值化为 16 种 quadrant/block 图形。 [生成器](../../packages/hya-tui-ts/scripts/generate-logo-art.py) | 对短、单色的 `Hya` 字形已经足够清晰；它不会保留抗锯齿、渐变或透明度。 |
| 更新 | 这是普通文本，交给现有 OpenTUI 文本渲染路径；没有额外图像状态或协议生命周期。 [LogoArt 组件数据](../../packages/hya-tui-ts/src/upstream/component/logo-art.data.ts) | 对静态启动页是最低风险、最低管理成本的方案。 |
| 可移植性 | 仅依赖 Unicode 字符和终端字体，不发送私有图像控制序列。Unicode 同时指出字符宽度和字体呈现不能由编码本身完全决定。 [UAX #11](https://www.unicode.org/reports/tr11/) | 仍需使用常见等宽终端字体做视觉测试，但它可作为所有图像协议失败时的确定性回退。 |

本研究**没有**运行吞吐、帧率、延迟或内存基准；不要把“默认最快”误读为已测得的
跨终端结论。

## 方案对比

| 方案 | 精度与缩放（已验证事实） | 更新与动画（已验证事实） | 支持/探测事实 | 对 hya 的判断（推断） |
| --- | --- | --- | --- | --- |
| **Unicode quadrant / half-block（现状）** | Unicode 定义 half-block、full-block 和 2 × 2 quadrant glyph。 [Unicode Block Elements](https://www.unicode.org/charts/nameslist/n_2580.html) | 正常重绘文本 cell。 | 无私有图像协议；字体覆盖仍影响形状。Unicode 的 East Asian Width 是宽度属性，不是统一像素画布。 [UAX #11](https://www.unicode.org/reports/tr11/) | 静态、小尺寸 hya 图标的默认和总回退；维持 48 × 16 子像素轮廓。 |
| **Unicode Braille** | Unicode 码位编码 dots 1–8；按通常 Braille 2 × 4 点阵解释，可用 8 个逻辑点/cell。 [Unicode Braille Patterns](https://www.unicode.org/charts/nameslist/n_2800.html) | 正常重绘文本 cell。 | 无图像协议，但需要字体有 Braille glyph。 | 可做细粒度单色数据图，不宜替换品牌实心 logo：点与点之间的间隙会改变边缘观感。这是视觉推断。 |
| **Unicode sextant（现代 cell 替代）** | Unicode 把 sextant 明确定义为“分为六部分”的 block mosaic terminal graphic characters。 [Unicode Symbols for Legacy Computing](https://www.unicode.org/charts/nameslist/n_1FB00.html) | 正常重绘文本 cell。 | 这批字符较新；Unicode 代码表明确说参考字形不是强制的，实际字体可以有明显差异。 [Unicode chart PDF](https://www.unicode.org/charts/PDF/U1FB00.pdf) | 可试验 2 × 3 单元格采样以提高纵向轮廓，但必须逐字体验收，不能取代 quadrant 的保守回退。 |
| **Kitty Graphics Protocol** | 协议目标就是任意 pixel raster、单像素定位、与文本上下叠放及 alpha；`c,r` 可把图缩放到指定 cell 矩形。 [Kitty protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) | 图像和 placement 都有 ID；同一 image/placement ID 的第二个 placement 会替换第一个，可用于移动/缩放而不闪烁。协议还定义 `a=f` 帧数据、矩形增量和终端驱动动画。 [Kitty placement and animation](https://sw.kovidgoyal.net/kitty/graphics-protocol/) | `a=q` 图像查询加 Primary Device Attributes 可确认支持；规范明确要求不要只凭猜测。 [Kitty detection](https://sw.kovidgoyal.net/kitty/graphics-protocol/) | **唯一建议的高保真 opt-in。** 首次上传一张小型预栅格 PNG/RGBA，缓存 image ID，resize 时替换 placement；不要为静态 logo 做动画。 |
| **iTerm2 inline images（OSC 1337）** | `File=...:base64` 支持 inline 图像，宽/高可为 cell、px、百分比或 `auto`，可选保比例；iTerm2 支持动画 GIF。 [iTerm2 Images](https://iterm2.com/documentation-images.html) | 文档化的单位是 File/MultipartFile 传输；它没有在该图像文档中给出 Kitty 式 image/placement ID 和帧增量 API。**因此“更新时重发图像”是推断，而非协议保证。** | iTerm2 要求用 Feature Reporting 探测。WezTerm 明确实现 iTerm2 inline image protocol，但也明确说明其 mux 会话尚未完全处理该协议。 [iTerm2 detection](https://iterm2.com/documentation-images.html) [WezTerm](https://wezterm.org/imgcat.html) | 有 macOS/iTerm2 或 WezTerm 用户需求时可做第二个 opt-in；勿以 `TERM_PROGRAM` 作为唯一依据。 |
| **SIXEL** | xterm 将 SIXEL 描述为 palette bitmap，基本元素是六个纵向像素；`DCS ... q` 发送图像。xterm 还可报告 SIXEL geometry（像素）与色寄存器。 [xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) | 所引的 `DCS ... q` 语法是一次图像流，不含 Kitty 式 named placement/frame API；把动画/局部更新实现为重传是**推断**。 | xterm 只有在配置成相应 DEC graphics 终端时才支持；Primary DA 的 `Ps=4` 表示 SIXEL，XTSMGRAPHICS 可以读取 SIXEL geometry/attributes。 [xterm SIXEL and probes](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) | 有价值的兼容分支，但首版不值得引入编码、量化、跨终端滚动/擦除差异和测试矩阵。 |

### 协议开销与生命周期

- **Kitty**：远端客户端必须 base64 后按不大于 4096 字节切片；本地协议还可选择文件或
  shared memory 传输。随后一个 image 可以多次 placement，且规范定义了终端在
  alternate screen/clear screen 时应清理图像。 [Kitty transfer, placement, lifecycle](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- **iTerm2**：普通 `File` 是 base64；为 tmux integration 的较大内容，iTerm2 定义
  `MultipartFile`/`FilePart`，并记录旧 tmux 序列上限 256 B、较新 tmux 与 iTerm2 上限
  1 MiB。 [iTerm2 multipart transport](https://iterm2.com/documentation-images.html)
- **SIXEL**：xterm 证据只承诺 palette bitmap/DCS 图像流；它没有给出一个可依赖的、
  跨终端的帧/placement 生命周期。因此不要假定其较 Kitty 更节省更新流量。
- **Unicode**：没有二进制图像上传；hya 只需要把现有短文本行交给 OpenTUI。对静态
  24 × 8 logo，这通常是最小的实现与状态成本；“通常最小”是**推断**，非字节基准。

## 推荐的探测与回退契约

不要根据 `$TERM`、终端品牌或 OS 直接选择图像协议。采用一次短超时的主动探测，按
TTY 会话缓存结果，并保留显式用户覆盖：

```text
forced cell ──────────────────────────────────────► quadrant fallback
auto ─► OpenTUI capability signal ─► protocol probe ─► kitty
                                              ├──────► iterm
                                              ├──────► sixel
                                              └──────► quadrant fallback
```

1. OpenTUI 0.3.4 的 `TerminalCapabilities` 已公开 `kitty_graphics`、`sixel`、
   `rgb`、`sgr_pixels` 和 `terminal` 字段，`CliRenderer` 公开只读
   `capabilities`。把它们当作候选信号。 [installed OpenTUI capability type](../../packages/hya-tui-ts/node_modules/@opentui/core/types.d.ts) [installed renderer API](../../packages/hya-tui-ts/node_modules/@opentui/core/renderer.d.ts)
2. 对 Kitty 仍发送规范的 `a=q` + DA 组合探测；只有先收到图像查询回应才启用。
   这是 Kitty 文档规定的判定方式。 [Kitty detection](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
3. 对 iTerm2 使用其 Feature Reporting，而不是环境变量猜测。 [iTerm2 inline-image detection](https://iterm2.com/documentation-images.html)
4. 对 SIXEL 使用 Primary DA（`Ps=4`）和/或 XTSMGRAPHICS 读取；若响应不完整或超时，
   直接回退。 [xterm probes](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
5. 在 tmux/screen/远端会话中默认保守。iTerm2 自己警告非标准 escape code 可能不能在
   tmux 或 screen 正常工作，WezTerm 也注明其 image protocol 尚未被 mux 完整处理。
   [iTerm2 warning](https://iterm2.com/documentation-escape-codes.html) [WezTerm warning](https://wezterm.org/imgcat.html)
6. 任意探测错误、超时、resize 后无法重新 placement、或用户指定 `cell`，都无条件回到
   已验证的 quadrant 文本图标；不得让 logo 拖慢或阻塞首帧。

## OpenTUI / Bun 与打包影响

- hya 当前锁定 `@opentui/core` / `@opentui/solid` **0.3.4**。
  [package manifest](../../packages/hya-tui-ts/package.json) 已安装版本的公开 renderable
  导出列表没有 `Image` renderable，而 renderer 的低层 `writeOut` 是私有成员。
  [renderable exports](../../packages/hya-tui-ts/node_modules/@opentui/core/renderables/index.d.ts) [renderer declaration](../../packages/hya-tui-ts/node_modules/@opentui/core/renderer.d.ts)
  因而目前不能把原始 Kitty/iTerm/SIXEL 字节简单当成一个现成 JSX `<image>` 元素；需要
  一个经过 OpenTUI 渲染周期协调的专用集成点。这是由当前 API 表面得出的**工程推断**。
- Bun 可以把 `Bun.file(...).bytes()` 读成 `Uint8Array`，也可以把数据写到 `Bun.stdout`。
  这说明协议 emitter 不必引入原生扩展，但**不**表示可以绕过 OpenTUI 的输出所有权。
  [Bun File I/O](https://bun.com/docs/runtime/file-io)
- 若增加像素图像路径，图标 PNG 必须要么嵌入 bundle（base64/byte array），要么明确复制到
  prepared runtime。当前做法把生成后的文本数据编进 TypeScript；不能假定开发树中的
  `docs/assets/hya-icon.png` 会随发布运行时存在。前半句是当前构建/生成器事实，后半句是
  发布安全性推断。 [build manifest](../../packages/hya-tui-ts/package.json) [generator](../../packages/hya-tui-ts/scripts/generate-logo-art.py)
- Kitty 分支还要持有 image/placement ID，并在退出、screen 切换和重新挂接时清理或重新
  传输；SIXEL 分支需 raster-to-SIXEL（含 palette）编码；iTerm2 分支需 base64 和必要的
  multipart 分片。这些是增加的维护面，而不是当前 cell 方案所需的依赖。

## 建议的交付次序

1. **现在**：保持 24 × 8 quadrant 输出；若要提高当前视觉质量，先在同一生成器中调节
   cell 预算并通过常见等宽字体截图验收，而非增加协议。
2. **需要大尺寸、抗锯齿、透明或品牌精确度时**：做 Kitty-only experimental renderer：
   `auto | cell | kitty` 设置、主动探测、PNG 嵌入、一个静态 placement、任何失败即回退。
3. **只在需求出现后**：为 iTerm2/WezTerm 添加单独 renderer；把 iTerm2 mux 情况纳入
   集成测试。
4. **最后才考虑 SIXEL**：仅当目标用户明确要求在已验证的 SIXEL 终端中使用图片时加入。
5. **验证矩阵**：cell（普通终端字体）；Kitty；iTerm2；WezTerm；xterm with/without
   SIXEL；每项分别在直连、tmux、SSH/remote 中验证探测、resize、alternate-screen
   cleanup、copy/scrollback 和失败回退。

## 一手来源清单

- [Kitty Terminal Graphics Protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [iTerm2 Inline Images Protocol](https://iterm2.com/documentation-images.html)
- [iTerm2 Proprietary Escape Codes](https://iterm2.com/documentation-escape-codes.html)
- [xterm Control Sequences — SIXEL and XTSMGRAPHICS](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [WezTerm iTerm Image Protocol](https://wezterm.org/imgcat.html)
- [Unicode Block Elements](https://www.unicode.org/charts/nameslist/n_2580.html)
- [Unicode Braille Patterns](https://www.unicode.org/charts/nameslist/n_2800.html)
- [Unicode Symbols for Legacy Computing](https://www.unicode.org/charts/nameslist/n_1FB00.html)
- [Unicode UAX #11: East Asian Width](https://www.unicode.org/reports/tr11/)
- [Bun File I/O](https://bun.com/docs/runtime/file-io)
- 当前项目与安装依赖的链接：
  [hya TUI package manifest](../../packages/hya-tui-ts/package.json)、
  [current logo generator](../../packages/hya-tui-ts/scripts/generate-logo-art.py)、
  [OpenTUI v0.3.4 declarations](../../packages/hya-tui-ts/node_modules/@opentui/core/types.d.ts)。
