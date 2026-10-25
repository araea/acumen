/* ============================================================================
   知微 · 控制台脚本
   ----------------------------------------------------------------------------
   单文件、零依赖、零构建：资源编译进二进制，界面不指望任何 CDN。

   几条纪律：
   - 拼 HTML 一律用 h`` 标签模板：插值默认转义，只有 raw() 包过的片段原样输出。
     页面上的字一半来自配置、日志与群友昵称，转义不能靠记得；
   - 不猜后端状态：写入以接口回执为准，失败就把控件留在原值并说明原因；
   - 能就地更新就不整页重画：整页重画会丢焦点、丢滚动、丢输入法状态；
   - 样式值只在 CSS 里。脚本只切类名与 ARIA 状态；
   - 手机上要一直流畅：日志合批、后台断流、动画交给 CSS 与令牌。

   §1 模板 · §2 图标 · §3 格式 · §4 口令与接口 · §5 反馈 · §6 外壳与路由 ·
   §7 总览 · §8 插件 · §8b 自动回复的规则面板 · §8c 潜水清理的名单面板 · §9 搭话资料（并进插件详情）· §10 日志 · §11 设置 · §11a 名单编辑器 · §11b 列表编辑器 · §11c 外观 · §12 解锁 · §13 事件 · §14 启动
   ========================================================================== */

(() => {
  "use strict";

  const NAME = "知微";
  const TOKEN_KEY = "acumen.token";
  const TIMEOUT = 20000;

  /* ==================== §1 模板 ==================== */

  const RAW = Symbol("raw");
  const raw = (html) => ({ [RAW]: String(html) });
  const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]);

  function part(value) {
    if (value === null || value === undefined || value === false) return "";
    if (Array.isArray(value)) return value.map(part).join("");
    if (typeof value === "object" && RAW in value) return value[RAW];
    return esc(value);
  }

  /** 标签模板：插值一律转义，数组逐项拼接，raw() 原样。 */
  function h(strings, ...values) {
    let out = strings[0];
    for (let i = 0; i < values.length; i++) out += part(values[i]) + strings[i + 1];
    return raw(out);
  }

  const put = (element, fragment) => {
    if (element) element.innerHTML = part(fragment);
  };
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const attr = (condition, name) => (condition ? raw(` ${name}`) : "");
  /** ARIA 的布尔状态要写成 "true" / "false"。h`` 会把 false 渲染成空串，
   *  aria-checked="" 等于没写：读屏读不出「关」，表单基线也会和后来写入的 "false" 对不上。 */
  const bool = (value) => (value ? "true" : "false");

  /* ==================== §2 图标 ==================== */
  /* Material Symbols Rounded，随页面内联在 index.html 的精灵图里（scripts/make-icons.py 生成）。
     图标只是装饰，一律对辅助技术隐藏，名字由旁边的文字或 aria-label 负责。
     `名字-fill` 是实心版：导航与状态符号成对出现，由 CSS 按选中态切换。
     字形是抄进每枚 <svg> 里的，不用 <use href="#名字">：不少手机自带浏览器与 App 内置浏览器
     画不出 <use> 引用的 symbol，图标整排空白。 */

  const GLYPHS = new Map();
  const glyph = (name) => {
    if (!GLYPHS.has(name)) {
      const symbol = document.getElementById(name);
      GLYPHS.set(name, symbol ? [symbol.getAttribute("viewBox") || "0 -960 960 960", symbol.innerHTML] : ["0 0 0 0", ""]);
    }
    return GLYPHS.get(name);
  };

  const icon = (name, extra = "") => {
    const [box, paths] = glyph(name);
    return raw(`<svg class="i${extra ? ` ${extra}` : ""}" viewBox="${box}" aria-hidden="true" focusable="false">${paths}</svg>`);
  };

  /** 一空一实两枚：选中时 CSS 只显示实心那枚。 */
  const duoIcon = (name) => h`${icon(name, "i-off")}${icon(`${name}-fill`, "i-on")}`;

  /** 每个插件一个自己的字形（一个概念一个图标：导航的「搭话」「日志」与插件同名者是同一枚）；
   *  没登记的新插件按分类给个兜底，色调（见 .tile）本来就跟着分类走。 */
  const PLUGIN_ICONS = {
    meta_filter: "filter_alt", ctl: "admin_panel_settings", console: "web", logger: "receipt_long",
    recorder: "database", media: "perm_media", sticker: "add_reaction", group_title: "workspace_premium",
    fake_title: "mood", recall: "undo", echo: "campaign", markdown: "markdown", repeater: "repeat",
    wordcloud: "cloud", stats: "leaderboard", gif: "gif_box", image_split: "grid_view",
    qr_scan: "qr_code_scanner", video_parse: "smart_display", song: "music_note",
    webshot: "screenshot_monitor", oai: "smart_toy", ambient: "forum", ai_news: "newspaper",
    help: "help", restart: "restart_alt", welcome: "waving_hand", anti_recall: "restore_page",
    nailong: "hide_image", autoreply: "quickreply", lurker: "scuba_diving", essence: "hub",
  };

  /* 分类不只是一种筛选，它先回答「这个插件是管什么的」。
     每个分类带一句人话说明；读屏与视觉读的是同一份，不在两处维护。 */
  const SECTION_META = {
    group: { icon: "groups", hint: "入群、潜水、精华与群规", name: "群管理" },
    chat: { icon: "forum", hint: "头衔、复读、回显与接话", name: "聊天互动" },
    media: { icon: "perm_media", hint: "图片、视频、点歌与截图", name: "媒体工具" },
    ai: { icon: "smart_toy", hint: "模型对话与资讯推送", name: "AI · 资讯" },
    data: { icon: "leaderboard", hint: "记录、统计与词云", name: "数据洞察" },
    system: { icon: "tune", hint: "运行、控制台与维护", name: "系统 · 运维" },
    misc: { icon: "extension", hint: "还没归类的插件", name: "其他" },
  };
  const sectionMeta = (code) => SECTION_META[code] || { icon: "extension", hint: "其它插件", name: "其他" };
  /* 筛选芯片只有一枚图标加一行字，两三个字最稳；完整名字仍用在列表标题、详情与读屏里。 */
  const SECTION_SHORT = { chat: "聊天", media: "媒体", ai: "AI", data: "数据", system: "系统" };
  const sectionLabel = (code, name) => SECTION_SHORT[code] || name;
  const SECTION_ICONS = Object.fromEntries(Object.entries(SECTION_META).map(([code, meta]) => [code, meta.icon]));
  const pluginGlyph = (plugin) => PLUGIN_ICONS[plugin.name] || SECTION_ICONS[plugin.section] || "extension";
  const pluginTile = (plugin, extra = "") =>
    h`<span class="tile${extra}" data-tone="${plugin.section}" aria-hidden="true">${icon(pluginGlyph(plugin))}</span>`;

  /* ==================== §3 格式 ==================== */

  const num = (value) => Number(value ?? 0).toLocaleString("zh-CN");

  function duration(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    const day = Math.floor(total / 86400);
    const hour = Math.floor((total % 86400) / 3600);
    const minute = Math.floor((total % 3600) / 60);
    if (day) return `${day} 天 ${hour} 小时`;
    if (hour) return `${hour} 小时 ${minute} 分`;
    if (minute) return `${minute} 分钟`;
    return "不到 1 分钟";
  }

  function ago(seconds) {
    const at = Number(seconds) || 0;
    if (!at) return "—";
    const stamp = at > 1e12 ? at : at * 1000;
    const delta = Math.floor((Date.now() - stamp) / 1000);
    if (delta < 60) return "刚刚";
    if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前`;
    if (delta < 86400) return `${Math.floor(delta / 3600)} 小时前`;
    if (delta < 86400 * 30) return `${Math.floor(delta / 86400)} 天前`;
    return new Date(stamp).toLocaleDateString("zh-CN");
  }

  const count = (text) => [...String(text)].length;

  /* ==================== §4 口令与接口 ==================== */

  const store = {
    get() {
      try {
        return localStorage.getItem(TOKEN_KEY) || "";
      } catch {
        return "";
      }
    },
    set(value) {
      try {
        localStorage.setItem(TOKEN_KEY, value);
      } catch {
        /* 隐私模式存不住：本次会话仍在内存里可用 */
      }
    },
    clear() {
      try {
        localStorage.removeItem(TOKEN_KEY);
      } catch {
        /* 同上 */
      }
    },
  };

  /** 地址栏里的 ?t= 读一次就抹掉：口令不该留在历史记录、截图与分享出去的链接里。 */
  function readToken() {
    const params = new URLSearchParams(location.search);
    const fromUrl = params.get("t");
    if (fromUrl) {
      store.set(fromUrl);
      history.replaceState(null, "", location.pathname + location.hash);
      return fromUrl;
    }
    return store.get();
  }

  let token = "";

  function failure(message, status = 0) {
    const error = new Error(message);
    error.status = status;
    return error;
  }

  async function api(path, { method = "GET", body } = {}) {
    const init = { method, headers: { "x-acumen-token": token }, cache: "no-store" };
    if (body !== undefined) {
      init.headers["content-type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), TIMEOUT);
    init.signal = abort.signal;
    let response;
    try {
      response = await fetch(`/api${path}`, init);
    } catch {
      throw failure(
        abort.signal.aborted
          ? "20 秒内没有回应，这台设备可能正忙。稍后再试。"
          : "连不上控制台。它可能已经停止，或者网络断开了。"
      );
    } finally {
      clearTimeout(timer);
    }
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      if (response.status === 401) lock("口令不正确，或者已经更换。请重新输入。");
      const error = failure(payload.error || `服务返回了 ${response.status}`, response.status);
      // 经管理面板打开、而知微没在运行：页面换成停机页，不当成一次失败。
      if (response.status === 503 && payload.offline) error.offline = true;
      throw error;
    }
    return payload;
  }

  /** 管理面板的接口（/panel/*）。只有经面板打开时才有；直接打开控制台时是 404。 */
  async function panelApi(path, { method = "GET" } = {}) {
    let response;
    try {
      response = await fetch(`/panel${path}`, { method, headers: { "x-acumen-token": token }, cache: "no-store" });
    } catch {
      throw failure("连不上管理面板。");
    }
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      if (response.status === 401) lock("口令不正确，或者已经更换。请重新输入。");
      throw failure(payload.error || `面板返回了 ${response.status}`, response.status);
    }
    return payload;
  }

  /** 接口失败的统一出口。401 已经换成解锁页，不再弹提示。 */
  function report(error) {
    if (error?.status === 401) return;
    snackbar(error?.message || String(error), "error");
  }

  /* ==================== §5 反馈 ==================== */

  const snack = { timer: 0, hold: false, left: 0, action: null };

  /** 提示条。成功与进行中是 status（礼貌播报），失败是 alert（立即播报）。
   *  鼠标悬停或键盘聚焦在提示条上时暂停计时（WCAG 2.2.1）。
   *  `action` 是 `{ label, run }`：提示条上的一个文字按钮，常用来「撤销」刚做的事。 */
  function snackbar(message, kind = "info", action = null) {
    clearTimeout(snack.timer);
    const host = $("#snackbar");
    put(
      host,
      h`<div class="snackbar${kind === "error" ? " snackbar-error" : ""}">
        <span class="snackbar-text" role="${kind === "error" ? "alert" : "status"}">${message}</span>
        ${action ? h`<button class="btn btn-text snackbar-action" type="button" data-snack-action>${action.label}</button>` : ""}
        <button class="icon-btn state" type="button" data-snack-close aria-label="关闭提示">${icon("close")}</button>
      </div>`
    );
    snack.action = action?.run ?? null;
    // 带「撤销」的多留一会儿：读完再决定需要时间（悬停与聚焦仍会暂停计时）。
    snack.left = kind === "error" ? Infinity : action ? 8000 : 4000;
    armSnack();
  }

  function armSnack() {
    clearTimeout(snack.timer);
    if (snack.hold || !Number.isFinite(snack.left) || !$("#snackbar .snackbar")) return;
    const started = Date.now();
    snack.timer = setTimeout(() => put($("#snackbar"), ""), snack.left);
    snack.started = started;
  }

  /** 页面本身已经把结果说清楚了（例如停机页），就别再留一条过时的「正在……」。 */
  function dismissSnack() {
    clearTimeout(snack.timer);
    snack.left = 0;
    put($("#snackbar"), "");
  }

  function holdSnack(hold) {
    if (snack.hold === hold) return;
    snack.hold = hold;
    if (hold) {
      clearTimeout(snack.timer);
      snack.left = Math.max(1500, snack.left - (Date.now() - (snack.started || Date.now())));
    } else armSnack();
  }

  /** 确认对话框：原生 dialog 负责焦点陷阱、Esc 与返回键；初始焦点落在「取消」。 */
  function confirmAction({ title, body, detail = "", confirm, cancel = "取消", danger = false }) {
    const dialog = $("#dialog");
    const opener = document.activeElement;
    put(
      dialog,
      h`${danger ? h`<span class="dialog-icon danger">${icon("warning-fill")}</span>` : ""}<h2 class="dialog-title" id="dialog-title">${title}</h2>
        <p class="dialog-body" id="dialog-body">${body}</p>
        ${detail ? h`<div class="dialog-notes" tabindex="0" role="region" aria-label="更新说明">${detail}</div>` : ""}
        <div class="actions actions-end">
          <button class="btn btn-text" type="button" value="cancel" autofocus>${cancel}</button>
          <button class="btn btn-filled${danger ? " btn-danger" : ""}" type="button" value="confirm">${confirm}</button>
        </div>`
    );
    return new Promise((resolve) => {
      const pick = (event) => {
        const button = event.target.closest("button[value]");
        if (button) dialog.close(button.value);
      };
      dialog.addEventListener("click", pick);
      dialog.addEventListener(
        "close",
        () => {
          dialog.removeEventListener("click", pick);
          if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
          resolve(dialog.returnValue === "confirm");
        },
        { once: true }
      );
      dialog.returnValue = "";
      dialog.showModal();
    });
  }

  /**
   * 停止的两条路：只停机器人（管理面板还活着，随时能再启动），或者连面板与守护一起停。
   * 默认选中轻的那一条；Esc 与「取消」都算没发生过。
   */
  function stopDialog() {
    const dialog = $("#dialog");
    const opener = document.activeElement;
    const panel = lifecycle.panel ? { ready: true } : powerInfo.panel;
    const safe = panel?.ready
      ? "管理面板继续运行，随时能在那里再启动知微。"
      : "管理面板没在运行；停止后要到 Termux 里运行 acumen start。";
    put(
      dialog,
      h`<span class="dialog-icon danger">${icon("warning-fill")}</span>
        <h2 class="dialog-title" id="dialog-title">停止知微？</h2>
        <p class="dialog-body" id="dialog-body">消息处理、对话与定时推送都会停下。选一种停法：</p>
        <fieldset class="choice-set">
          <legend class="visually-hidden">停止范围</legend>
          <label class="choice">
            <input type="radio" name="stop-scope" value="bot" checked>
            <span class="choice-text">
              <span class="choice-title">仅停止机器人</span>
              <span class="choice-note">${safe}</span>
            </span>
          </label>
          <label class="choice">
            <input type="radio" name="stop-scope" value="all">
            <span class="choice-text">
              <span class="choice-title">全面停止所有服务</span>
              <span class="choice-note">机器人、管理面板与开机守护一起停，这个网页也会关掉。之后要在 Termux 里运行 acumen start。</span>
            </span>
          </label>
        </fieldset>
        <div class="actions actions-end">
          <button class="btn btn-text" type="button" value="cancel">取消</button>
          <button class="btn btn-filled btn-danger" type="button" value="confirm">停止</button>
        </div>`
    );
    return new Promise((resolve) => {
      const pick = (event) => {
        const button = event.target.closest("button[value]");
        if (button) dialog.close(button.value);
      };
      dialog.addEventListener("click", pick);
      dialog.addEventListener(
        "close",
        () => {
          dialog.removeEventListener("click", pick);
          const chosen = dialog.returnValue === "confirm"
            ? dialog.querySelector('input[name="stop-scope"]:checked')?.value || null
            : null;
          if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
          resolve(chosen);
        },
        { once: true }
      );
      dialog.returnValue = "";
      dialog.showModal();
      $('input[name="stop-scope"]:checked', dialog)?.focus();
    });
  }

  const emptyState = (message, name = "inbox") =>
    h`<div class="empty"><span class="empty-mark">${icon(name)}</span><p>${message}</p></div>`;

  const skeleton = () =>
    h`<div class="page" aria-hidden="true"><div class="skeleton">
        <div class="skeleton-block"></div><div class="skeleton-block skeleton-tall"></div>
        <div class="skeleton-block"></div><div class="skeleton-block"></div>
      </div></div>`;

  function pageHead(title, sub = "", { back = null, actions = "", lead = "", meta = "" } = {}) {
    return h`<header class="page-head">
      <div class="page-head-text">
        ${back ? h`<a class="btn btn-text back" href="${back.href}">${icon("arrow_back")}${back.label}</a>` : ""}
        ${lead
          ? h`<div class="page-lead">${lead}<h1 class="page-title" tabindex="-1">${title}</h1></div>`
          : h`<h1 class="page-title" tabindex="-1">${title}</h1>`}
        ${sub ? h`<p class="page-sub">${sub}</p>` : ""}
        ${meta ? h`<div class="page-meta">${meta}</div>` : ""}
      </div>
      ${actions ? h`<div class="page-actions">${actions}</div>` : ""}
    </header>`;
  }

  const statusTag = (plugin) =>
    h`<span class="detail-status" data-status-for="${plugin.name}">${statusBadge(plugin)}</span>`;

  const statusBadge = (plugin) =>
    plugin.pending
      ? h`<span class="status status-warning">待重启</span>`
      : plugin.on
        ? h`<span class="status status-success">已启用</span>`
        : h`<span class="status">已停用</span>`;

  /* ==================== §6 外壳与路由 ==================== */

  // 搭话（persona / 记忆 / 表情包）是 ambient 插件的一部分，收进插件详情，不再单独占一个导航入口。
  const PAGES = [
    { id: "overview", label: "总览", glyph: "dashboard" },
    { id: "plugins", label: "插件", glyph: "extension" },
    { id: "logs", label: "日志", glyph: "receipt_long" },
    { id: "settings", label: "设置", glyph: "settings" },
  ];

  // 840px 起左侧导航会展开成 264px，留给内容的宽度不够并排摆列表与详情；
  // 到 1000px 再切双栏，平板竖屏与 840–999 的窗口仍走单栏详情页。
  const wide = matchMedia("(min-width: 1000px)");
  const compactNav = matchMedia("(max-width: 599.98px)");

  function buildShell() {
    put(
      $("#nav"),
      h`<span class="nav-pill" aria-hidden="true"></span>${PAGES.map(
        (page) => h`<a class="nav-item" href="#/${page.id}" data-nav="${page.id}" draggable="false">
          <span class="nav-icon state">${duoIcon(page.glyph)}</span>
          <span class="nav-label">${page.label}</span>
        </a>`
      )}`
    );
    put(
      $("#topbar-actions"),
      h`<span id="update-slot"></span><button class="icon-btn state" id="refresh-page" type="button" aria-label="刷新当前页面" title="刷新当前页面">${icon("refresh")}</button>`
    );
  }

  /* 胶囊跟着当前格走。切换页面时先记住它现在的位置，再在 ~380ms 内
     逐帧追到新格（新格自己也在随 flex 过渡伸展），所以胶囊像被「带」过去，
     不是凭空跳过去。首帧、缩放、断点切换与减少动态都直接落位。
     滑动途中它是一枚液态玻璃透镜（见 app.css 的 .nav[data-glass]）：按速度拉长，
     落格时回弹一下再凝回实心。 */
  let navPillFrame = 0;
  const calm = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

  /** 把盒子的位置与尺寸写进胶囊。透镜的形变也走这里，不碰 CSS 的 scale。 */
  function placePill(pill, box) {
    pill.style.transform = `translate3d(${box.x.toFixed(2)}px, ${box.y.toFixed(2)}px, 0)`;
    pill.style.width = `${Math.max(0, box.w).toFixed(2)}px`;
    pill.style.height = `${Math.max(0, box.h).toFixed(2)}px`;
  }

  /** 玻璃按速度变形：快了横向拉长、纵向收一点，体积大致守恒。v 是每毫秒像素。
   *
   *  形变直接算进盒子（宽高与位移），不用 CSS 的 `scale`：Chromium 在同一个元素上
   *  同时写 transform 与 scale 时，缩放后的盒子会被算到 translate 之外（实测在 390px
   *  宽的屏幕里能跑到 x≈386px），拖动到最后一格时整枚玻璃越过屏幕右缘。改盒子的
   *  宽高没有这个问题，也能顺手把它按在导航坞里面：贴到边上时向里堆，不伸出去。 */
  function stretchPill(pill, box, velocity, bounds) {
    if (!box.w) return placePill(pill, box);
    const stretch = Math.min(1.32, 1 + Math.abs(velocity) * 0.22);
    const w = Math.min(box.w * stretch, bounds.right - bounds.left);
    const h = box.h * Math.sqrt(box.w / w);
    const x = Math.min(
      Math.max(box.x - (w - box.w) / 2, bounds.left),
      bounds.right - w
    );
    placePill(pill, { x, y: box.y - (h - box.h) / 2, w, h });
  }

  /** 落格：玻璃凝回实心，高光边缘轻轻回弹一下。 */
  function settlePill(nav, pill) {
    delete nav.dataset.glass;
    if (calm()) return;
    // 回弹画在 ::after 上：它只是一层高光，没有布局，也就不会再牵扯胶囊的位置。
    pill.classList.remove("nav-settle");
    void pill.offsetWidth;
    pill.classList.add("nav-settle");
  }
  /* 胶囊是一枚真弹簧（M3E spatial：stiffness 600、damping 0.72，约 4% 回弹），不是定长的
     缓动：每帧量一次当前格（它自己也在随 flex 伸展），按弹簧积分追过去。连点几格时
     不从头再来，而是带着当前的速度转向——胶囊像被手推着走，不会一顿一顿。 */
  const NAV_SPRING = { stiffness: 600, damping: 0.72 };
  let navSpring = null;

  function stepSpring(axis, target, dt) {
    const w0 = Math.sqrt(NAV_SPRING.stiffness);
    const force = -NAV_SPRING.stiffness * (axis.p - target) - 2 * NAV_SPRING.damping * w0 * axis.v;
    axis.v += force * dt;
    axis.p += axis.v * dt;
  }

  function paintNavPill(animate = true, glass = false) {
    const nav = $("#nav");
    const pill = nav?.querySelector(".nav-pill");
    const item = nav?.querySelector('.nav-item[aria-current="page"]');
    if (!nav || !pill || !item) return;

    const measure = (element) => {
      const navRect = nav.getBoundingClientRect();
      const rect = element.getBoundingClientRect();
      return {
        x: rect.left - navRect.left - nav.clientLeft,
        y: rect.top - navRect.top - nav.clientTop,
        w: rect.width,
        h: rect.height,
      };
    };
    const first = !pill.dataset.painted;
    pill.dataset.painted = "1";
    const moving = animate && !first && !calm();
    if (!moving) {
      cancelAnimationFrame(navPillFrame);
      navSpring = null;
      delete nav.dataset.glass;
      placePill(pill, measure(item));
      return;
    }

    // 弹簧没在跑：从胶囊现在画着的位置出发（也可能是拖选松手的地方），速度归零。
    if (!navSpring) {
      const from = measure(pill);
      navSpring = {
        x: { p: from.x, v: 0 }, y: { p: from.y, v: 0 },
        w: { p: from.w, v: 0 }, h: { p: from.h, v: 0 },
      };
    }
    // 只有手机底栏上换了格才化成玻璃；导航轨上下换格、底栏收起展开照旧是实心滑块。
    const glide = glass && compactNav.matches && Math.abs(measure(item).x - navSpring.x.p) > 8;
    // 格子的 flex 过渡要跑完 --zw-nav-duration，弹簧至少跟这么久，免得追到半路的目标就停。
    const minimum = parseFloat(getComputedStyle(nav).getPropertyValue("--zw-nav-duration")) || 0;
    const start = performance.now();
    let last = start;
    if (glide) nav.dataset.glass = "glide";
    else delete nav.dataset.glass;
    const cs = getComputedStyle(nav);
    const bounds = {
      left: parseFloat(cs.paddingLeft) || 0,
      right: nav.clientWidth - (parseFloat(cs.paddingRight) || 0),
    };

    cancelAnimationFrame(navPillFrame);
    const step = (now) => {
      const target = measure(nav.querySelector('.nav-item[aria-current="page"]') || item);
      let dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      // 半隐式欧拉，切成 4ms 一小步：低帧率时也稳，不会弹飞。
      while (dt > 0) {
        const h = Math.min(dt, 0.004);
        for (const key of ["x", "y", "w", "h"]) stepSpring(navSpring[key], target[key], h);
        dt -= h;
      }
      const s = navSpring;
      const box = { x: s.x.p, y: s.y.p, w: s.w.p, h: s.h.p };
      const settled = ["x", "y", "w", "h"].every((key) => Math.abs(s[key].p - target[key]) < 0.3 && Math.abs(s[key].v) < 8);
      if (settled && now - start >= minimum) {
        navSpring = null;
        placePill(pill, target);
        if (glide) settlePill(nav, pill);
        return;
      }
      if (glide) {
        stretchPill(pill, box, s.x.v / 1000, bounds);
        pill.style.setProperty("--zw-glass-x", `${Math.round(50 + Math.sign(s.x.v) * 18)}%`);
      } else {
        placePill(pill, box);
      }
      navPillFrame = requestAnimationFrame(step);
    };
    navPillFrame = requestAnimationFrame(step);
  }

  /* 尺寸/断点变了才重新落位；换页动画进行中的偶发 observer 回调不打断它。 */
  function syncNavPill() {
    const nav = $("#nav");
    const pill = nav?.querySelector(".nav-pill");
    if (!nav || !pill || nav.classList.contains("nav-dragging")) return;
    const rect = nav.getBoundingClientRect();
    const key = `${rect.width.toFixed(1)}x${rect.height.toFixed(1)}`;
    if (pill.dataset.navSize === key && pill.dataset.painted) return;
    pill.dataset.navSize = key;
    cancelAnimationFrame(navPillFrame);
    nav.classList.add("nav-snap");
    paintNavPill(false);
    requestAnimationFrame(() => nav.classList.remove("nav-snap"));
  }

  /* —— 长按拖选（compact） ——
     按住底栏不松手，五格先摊成等宽，胶囊跟着手指走；松手落在最近的一格并
     展开。短按、键盘与桌面导航轨都不受影响。 */
  function bindNavDrag() {
    const nav = $("#nav");
    if (!nav) return;
    let drag = null;
    let suppressClickUntil = 0;

    const items = () => $$(".nav-item", nav);
    const activeId = () => nav.querySelector('.nav-item[aria-current="page"]')?.dataset.nav || "";
    const setCurrent = (id) => {
      for (const item of items()) {
        if (item.dataset.nav === id) item.setAttribute("aria-current", "page");
        else item.removeAttribute("aria-current");
      }
    };

    /* 摊平之后的几何：五格等宽，胶囊宽高与一格一致。 */
    const metrics = () => {
      const cs = getComputedStyle(nav);
      const padL = parseFloat(cs.paddingLeft) || 0;
      const padR = parseFloat(cs.paddingRight) || 0;
      const gap = parseFloat(cs.columnGap) || 0;
      const list = items();
      const content = nav.clientWidth - padL - padR;
      const cell = Math.max(1, (content - gap * (list.length - 1)) / list.length);
      const rect = nav.getBoundingClientRect();
      const first = rect.left + nav.clientLeft + padL + cell / 2;
      const centers = list.map((_, i) => first + i * (cell + gap));
      return {
        cell, gap, centers, rect,
        top: list[0].getBoundingClientRect().top - rect.top - nav.clientTop,
        height: list[0].getBoundingClientRect().height,
        // 玻璃不能越过的左右边界（相对 padding box），与胶囊的坐标同一个原点。
        left: padL,
        right: nav.clientWidth - padR,
      };
    };

    const paintFree = (x) => {
      if (!drag) return;
      const m = metrics();
      const cx = Math.min(m.centers[m.centers.length - 1], Math.max(m.centers[0], x));
      const nearest = m.centers.reduce((best, c, i) =>
        Math.abs(c - cx) < Math.abs(m.centers[best] - cx) ? i : best, 0);
      const id = items()[nearest].dataset.nav;
      if (id !== drag.target) {
        drag.target = id;
        setCurrent(id);
        if (navigator.vibrate) navigator.vibrate(5);
      }
      const pill = nav.querySelector(".nav-pill");
      const localX = cx - (m.rect.left + nav.clientLeft) - m.cell / 2;
      const box = { x: localX, y: m.top, w: m.cell, h: m.height };
      if (calm()) {
        placePill(pill, box);
        return;
      }
      const now = performance.now();
      // 速度取短时平均，手指一顿玻璃就慢慢缩回圆润，不会一抖一抖。
      const instant = drag.lastX === undefined ? 0 : (x - drag.lastX) / Math.max(1, now - drag.lastT);
      drag.velocity = (drag.velocity ?? 0) * 0.6 + instant * 0.4;
      drag.lastX = x;
      drag.lastT = now;
      stretchPill(pill, box, drag.velocity, m);
      const within = Math.min(1, Math.max(0, (x - (m.rect.left + nav.clientLeft + localX)) / m.cell));
      pill.style.setProperty("--zw-glass-x", `${Math.round(within * 100)}%`);
    };

    const cleanup = () => {
      if (drag?.timer) clearTimeout(drag.timer);
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
      removeEventListener("pointercancel", cancel);
      removeEventListener("blur", cancel);
      drag = null;
    };

    const activate = (x) => {
      if (!drag || drag.active || !compactNav.matches) return;
      drag.active = true;
      clearTimeout(drag.timer);
      cancelAnimationFrame(navPillFrame);
      navSpring = null;
      nav.classList.add("nav-dragging");
      if (!calm()) nav.dataset.glass = "drag";
      paintFree(x);
      if (navigator.vibrate) navigator.vibrate(8);
    };

    const move = (event) => {
      if (!drag || event.pointerId !== drag.id) return;
      const dx = event.clientX - drag.x0;
      const dy = event.clientY - drag.y0;
      if (!drag.active) {
        // 只把「明显竖直」的移动当滚动让开；手指的轻微抖动不取消拖选。
        if (Math.abs(dy) > 24 && Math.abs(dy) > Math.abs(dx) * 2) return cleanup();
        if (Math.abs(dx) < 8) return;
        activate(event.clientX);
      }
      if (!drag?.active) return;
      if (event.cancelable) event.preventDefault();
      paintFree(event.clientX);
    };

    const up = (event) => {
      if (!drag || event.pointerId !== drag.id) return;
      const state = drag;
      cleanup();
      if (!state.active) {
        // 触摸短按：click 可能已被 preventDefault 吃掉，这里自己导航一次。
        if (state.pointerType !== "mouse") {
          suppressClickUntil = performance.now() + 150;
          if (state.target !== state.original) location.hash = "#/" + state.target;
        }
        return;
      }
      // 拖完松手时浏览器还会补一个 click：吃掉它，只走这里的落格。
      suppressClickUntil = performance.now() + 150;
      nav.classList.remove("nav-dragging");
      const rect = nav.getBoundingClientRect();
      if (event.clientY < rect.top - 48 || event.clientY > rect.bottom + 48) {
        setCurrent(state.original);
        paintNavPill(true, true);
        return;
      }
      paintNavPill(true, true);
      if (state.target !== state.original) location.hash = "#/" + state.target;
    };

    const cancel = () => {
      if (!drag) return;
      const state = drag;
      cleanup();
      if (!state.active) return;
      nav.classList.remove("nav-dragging");
      setCurrent(state.original);
      paintNavPill(true, true);
    };

    // Android 的长按菜单与 iOS 的链接预览都从 touchstart 的默认动作开始；
    // 这里一并关掉，tap 由上面的 pointerup 自己落格。
    nav.addEventListener("touchstart", (event) => {
      // pointerdown 先到（drag 已建立）才接管；不支持 Pointer Events 的浏览器
      // 不会进这里，链接的原生 click 仍然可用。
      if (drag && compactNav.matches && event.cancelable) event.preventDefault();
    }, { passive: false });

    nav.addEventListener("pointerdown", (event) => {
      if (!compactNav.matches || drag) return;
      if (event.pointerType === "mouse" && event.button !== 0) return;
      const item = event.target.closest(".nav-item");
      if (!item) return;
      drag = {
        id: event.pointerId,
        pointerType: event.pointerType,
        x0: event.clientX,
        y0: event.clientY,
        active: false,
        original: activeId(),
        target: item.dataset.nav,
        timer: 0,
      };
      // 触摸 / 手写笔：从按下这一刻就接管，别让浏览器把长按解释成
      // 文本选择、链接拖拽或系统菜单。短按由 up 里自己落格。
      if (event.pointerType !== "mouse") event.preventDefault();
      drag.timer = setTimeout(() => activate(drag.x0), 180);
      addEventListener("pointermove", move, { passive: false });
      addEventListener("pointerup", up);
      addEventListener("pointercancel", cancel);
      addEventListener("blur", cancel);
    });

    nav.addEventListener("click", (event) => {
      if (performance.now() < suppressClickUntil) {
        event.preventDefault();
        event.stopPropagation();
        suppressClickUntil = 0;
      }
    }, true);

    nav.addEventListener("contextmenu", (event) => {
      if (drag) event.preventDefault();
    });

    compactNav.addEventListener("change", () => {
      if (!compactNav.matches && drag) cancel();
    });
  }

  function markNav(active) {
    const nav = $("#nav");
    if (!nav) return;
    const previous = nav.querySelector('.nav-item[aria-current="page"]');
    const next = nav.querySelector(`[data-nav="${active}"]`);
    const animate = Boolean(previous && previous !== next);
    for (const item of $$("#nav [data-nav]", nav)) {
      if (item === next) item.setAttribute("aria-current", "page");
      else item.removeAttribute("aria-current");
      item.removeAttribute("data-pop");
    }
    if (animate) {
      // 新格的图标弹一下（app.css 的 nav-pop）；动画放完就摘掉，下次换回来还能再播。
      next?.setAttribute("data-pop", "");
      next?.querySelector(".nav-icon")?.addEventListener("animationend", () => next.removeAttribute("data-pop"), { once: true });
      paintNavPill(true, true);
    } else syncNavPill();
  }

  /* —— 滚动收起（compact） ——
     往下读超过一屏顶部的 64px，并且这一下是往下滚了一截，底栏就收矮一点；往上滚、
     回到页顶、或者焦点进了底栏，就恢复。坞的高度变化由 CSS 过渡，胶囊跟着重新量。 */
  function bindNavMini() {
    const nav = $("#nav");
    if (!nav) return;
    let lastY = scrollY;
    let ticking = false;
    const set = (mini) => {
      if (nav.hasAttribute("data-mini") === mini) return;
      nav.toggleAttribute("data-mini", mini);
      paintNavPill(true);
    };
    addEventListener("scroll", () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        ticking = false;
        const y = scrollY;
        const delta = y - lastY;
        if (!compactNav.matches || nav.classList.contains("nav-dragging") || nav.matches(":focus-within")) {
          lastY = y;
          return set(false);
        }
        if (y < 64) set(false);
        else if (delta > 12) set(true);
        else if (delta < -12) set(false);
        if (Math.abs(delta) > 12 || y < 64) lastY = y;
      });
    }, { passive: true });
    nav.addEventListener("focusin", () => set(false));
    compactNav.addEventListener("change", () => set(false));
  }

  /* ==================== §6c 手势翻页 ====================
     左右滑整页：手指到哪儿，页面到哪儿，底部导航的胶囊同步走到下一格；松手时按
     「过了小半屏」或「甩了一下」决定去还是回。落页不是一段定长缓动，而是一枚
     接着松手那一刻速度走的弹簧（见 swipeSettle）：慢推慢收、快甩走完，中途没有
     「停一下再走」的那一顿。

     三件事值得记下来：

     - **下一张纸是画在固定层里的。** 目标页得在手指还没到之前就看得见，所以滑动开始时
       把目标页画进一层 position: fixed 的纸里：横向跟手，纵向按它自己上次的滚动位置摆好。
       落页时真正的页面已经画在下面，撤掉这一层就完成，不会闪一下白。正页一边走一边
       退后 3%（缩放），两张纸因此有前后；落页时它整张移出屏幕，这点缩放看不见。
     - **不碰 touch-action 的老规矩。** body 是 manipulation、导航坞是 none；这里只给
       .main 设 pan-y：竖直留给浏览器滚动，横向留给我们，pinch 缩放也还在（WCAG 1.4.4）。
       横滑的容器（芯片、选项卡、模板位）在 app.css 里各自把 touch-action 改回 auto。
     - **拖动中不改 aria-current。** 只让胶囊跟手——它是视觉焦点；落页那一下才换选中格，
       读屏不会因为手指抖一下就报一次「已选中」。 */

  const SWIPE_GATE = 10;        // 横向走过这么多才算一次滑动，短按仍然是短按
  const SWIPE_TILT = 1.15;      // 竖向超过横向的这个倍数，就当用户是在滚动
  const swipe = {
    drag: null,
    layer: null,
    sheet: null,
    scrim: null,
    scrollEl: null,
    frame: 0,
    gen: 0,
    settling: false,
    suppressClickUntil: 0,
  };

  const viewportW = () => document.documentElement.clientWidth || innerWidth;

  const swipeNeighbor = (page, step) => {
    const index = PAGES.findIndex((item) => item.id === page);
    return index < 0 ? null : PAGES[index + step]?.id ?? null;
  };

  /** 边缘上的橡皮筋：越拉越沉，最多走两成屏宽。 */
  const rubber = (dx, width) =>
    Math.sign(dx) * width * 0.2 * (1 - Math.exp(-Math.abs(dx) / (width * 0.34)));

  function swipeAvailable() {
    if (!token || lifecycle.stopped || swipe.settling || swipe.drag || !shown) return false;
    // 插件详情是「列表 → 详情」的下一层，横滑退出会和返回手势打架，不参与翻页。
    if (shown.arg) return false;
    if (!$("#view") || $("dialog[open]")) return false;
    return true;
  }

  /** 手指底下的东西自己会横滑、会选中文字、会弹键盘：都让开。 */
  const SWIPE_SKIP = 'input, textarea, select, [contenteditable], [data-swipe="off"], .chips-scroll, .tabs, .tpl-holders';

  function swipeSurface() {
    const layer = document.createElement("div");
    layer.className = "swipe-layer";
    layer.setAttribute("aria-hidden", "true");
    layer.inert = true;
    layer.innerHTML = '<div class="swipe-scrim"></div><div class="swipe-sheet"><div class="swipe-scroll"><div class="view"></div></div></div>';
    document.body.appendChild(layer);
    swipe.layer = layer;
    swipe.sheet = layer.querySelector(".swipe-sheet");
    swipe.scrim = layer.querySelector(".swipe-scrim");
    swipe.scrollEl = layer.querySelector(".swipe-scroll");
    return layer;
  }

  /** 把目标页画进那张纸。骨架先到位，数据回来再换——手指滑到一半时纸里已经有东西。 */
  function swipePaint(target) {
    if (!swipe.scrollEl) return;
    const box = swipe.scrollEl.querySelector(".view");
    const route = { page: target, arg: null, key: target };
    const fill = (node) => {
      if (box.isConnected) put(box, node);
    };
    put(box, skeleton());
    PAINT[target](route).then(fill, (error) => {
      if (error?.status === 401) return;
      fill(h`<div class="page">${pageHead("没能打开这一页")}</div>`);
    });
  }

  function swipeLock(d, dx) {
    d.step = dx < 0 ? 1 : -1;
    d.target = swipeNeighbor(shown.page, d.step);
    d.locked = true;
    d.keep = d.target ? scrolls.get(d.target) || 0 : 0;
    d.view.style.willChange = "transform, scale";
    d.view.dataset.swiping = "";
    // 起止两格的盒子一次量完：途中每帧再量一次，等于每帧强制重算一遍布局。
    d.pill = d.target ? swipePillBoxes(shown.page, d.target) : null;
    if (!d.target) return;

    // 下一张纸的纵向原点：.view 在文档里的位置。滚动到哪儿都不变，
    // 所以纸里的内容按目标页自己的滚动位置（-keep）摆一次就够。
    const rect = d.view.getBoundingClientRect();
    const layer = swipeSurface();
    layer.style.top = `${Math.max(0, rect.top + scrollY)}px`;
    swipe.scrollEl.style.transform = `translate3d(0, ${-d.keep}px, 0)`;
    swipe.sheet.style.transform = `translate3d(${d.step * viewportW()}px, 0, 0)`;
    swipePaint(d.target);
  }

  /** 一帧的位移：正页、下一张纸、压暗层、导航胶囊，一起走。
   *  这里只写 transform 与 opacity，量尺寸的事全在 swipeLock 里做过一次。 */
  function swipePaintAt(d, out) {
    if (!d?.locked) return;
    const width = viewportW();
    const k = Math.min(1, Math.abs(out) / width);
    d.painted = out;
    // 正页跟着手指走，同时往后退一点（最多 3%）：两张纸有了前后，滑动不再像搬一块板。
    // 落页时正页整张移出屏幕，这点缩放看不见，所以收尾不会跳一下。
    const depth = d.target ? 0.03 * k : 0;
    d.view.style.transform = depth
      ? `translate3d(${out.toFixed(2)}px, 0, 0) scale(${(1 - depth).toFixed(4)})`
      : `translate3d(${out.toFixed(2)}px, 0, 0)`;
    if (!swipe.sheet) {
      if (swipe.scrim) swipe.scrim.style.opacity = (k * 0.12).toFixed(3);
      return;
    }
    swipe.sheet.style.transform = `translate3d(${(d.step * width + out).toFixed(2)}px, 0, 0)`;
    if (swipe.scrim) swipe.scrim.style.opacity = (k * 0.18).toFixed(3);
    swipePill(k, d.pill);
  }

  function swipePaintFrame() {
    const d = swipe.drag;
    if (!d?.locked) return;
    swipePaintAt(d, d.target ? d.offset : rubber(d.offset, viewportW()));
  }

  function navBox(element) {
    const nav = $("#nav");
    const navRect = nav.getBoundingClientRect();
    const rect = element.getBoundingClientRect();
    return {
      x: rect.left - navRect.left - nav.clientLeft,
      y: rect.top - navRect.top - nav.clientTop,
      w: rect.width,
      h: rect.height,
    };
  }

  /** 拖动开始就量好起止两格：途中每帧再量会把布局重算一遍，白掉一帧。 */
  function swipePillBoxes(fromId, toId) {
    const nav = $("#nav");
    const from = nav?.querySelector(`[data-nav="${CSS.escape(fromId)}"]`);
    const to = nav?.querySelector(`[data-nav="${CSS.escape(toId)}"]`);
    if (!nav || !from || !to) return null;
    return { nav, from: navBox(from), to: navBox(to) };
  }

  /** 胶囊在两格之间按进度插值。位与尺寸都插，宽窄不同也接得上。 */
  function swipePill(t, boxes) {
    if (!boxes) return;
    const { nav, from: a, to: b } = boxes;
    const pill = nav.querySelector(".nav-pill");
    if (!pill) return;
    const k = Math.min(1, Math.max(0, t));
    const box = {
      x: a.x + (b.x - a.x) * k,
      y: a.y + (b.y - a.y) * k,
      w: a.w + (b.w - a.w) * k,
      h: a.h + (b.h - a.h) * k,
    };
    // 胶囊离开之后，原来那格的标签跟着收（app.css 的 .nav[data-swipe]）：
    // 悬浮的药丸滑到一半时，字不该还探在丸子的左沿外面。
    nav.dataset.swipe = "";
    nav.style.setProperty("--zw-swipe", k.toFixed(3));
    // 翻页时不化成液态玻璃：那是一枚跟着手指的「焦点」，不是被端起来的透镜。
    // 实心药丸在两格之间走，压在哪一格上，那一格的图标就透出来，一眼看得出走到哪了。
    placePill(pill, box);
  }

  /* 松手之后：一枚带初速的弹簧接着手指的速度走。
     定长缓动会从零速重新起步——平移到最后那一下「顿住再走」就是它；
     弹簧把松手那一刻的速度原样接上：慢慢拖到一半就轻轻收回去，甩一下则顺着劲落到位。
     阻尼取 0.88（接近临界）：不越界，纸的边不会露出来；刚度 520 让整页的落位大约
     300ms、七十几像素的回弹约 240ms——比 M3 的 350ms 空间缓动略紧，手感更「跟手」。 */
  const SWIPE_SPRING = { stiffness: 520, damping: 0.88 };

  function swipeSettle(d, to, velocity, gen) {
    const from = d.painted ?? 0;
    return new Promise((done) => {
      const land = () => {
        swipePaintAt(d, to);
        done();
      };
      if (calm() || Math.abs(to - from) < 0.5) return land();
      const w0 = Math.sqrt(SWIPE_SPRING.stiffness);
      // 只夹住越过终点的那一边：回弹时别滑过 0（页边会露出底色），落页时别越过终点。
      // 另一边留给初速——松手那一刻手指还在动，步子本来就该先冲出去一点。
      const stop = (x) => (to < 0 ? Math.max(to, x) : Math.min(to, x));
      let p = from;
      let v = Number(velocity) || 0;
      // 第一帧只用来对齐时间：从松手到第一帧之间隔了多久，不该一次性补进位移里，
      // 那样就成了「一帧跳到终点」。掉帧时单帧最多补 32ms，宁可慢一点也不要跳。
      let last = 0;
      const step = (now) => {
        // 被后来的手势接管了：这一趟就地收手，收尾交给接管者。
        if (gen !== swipe.gen) return;
        if (!last) { last = now; requestAnimationFrame(step); return; }
        let dt = Math.min(0.032, Math.max(0, (now - last) / 1000));
        last = now;
        // 半隐式欧拉，切成 4ms 一小步：掉帧时也稳，不会弹飞。
        while (dt > 0) {
          const h = Math.min(dt, 0.004);
          v += (-SWIPE_SPRING.stiffness * (p - to) - 2 * SWIPE_SPRING.damping * w0 * v) * h;
          p += v * h;
          dt -= h;
        }
        // 差不到一像素、也快停下来了，就直接落到终点：最后那几像素的尾巴最拖时间。
        if (Math.abs(p - to) < 0.6 && Math.abs(v) < 40) return land();
        swipePaintAt(d, stop(p));
        requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    });
  }

  function swipeCleanup() {
    if (swipe.frame) cancelAnimationFrame(swipe.frame);
    swipe.frame = 0;
    swipe.layer?.remove();
    swipe.layer = null;
    swipe.sheet = null;
    swipe.scrim = null;
    swipe.scrollEl = null;
    const view = $("#view");
    if (view) {
      view.style.transform = "";
      view.style.willChange = "";
      delete view.dataset.swiping;
    }
    const nav = $("#nav");
    if (nav) {
      delete nav.dataset.swipe;
      nav.style.removeProperty("--zw-swipe");
    }
  }

  async function swipeCommit(d) {
    const gen = ++swipe.gen;
    swipe.settling = "go";
    const route = { page: d.target, arg: null, key: d.target };
    const keep = d.keep;
    // 先把页滑完，落页那一下才换选中格：手指还在走的时候，读屏不该先报「已选中」。
    await swipeSettle(d, -d.step * viewportW(), d.release, gen);
    if (gen !== swipe.gen) return;
    if (navigator.vibrate && !calm()) navigator.vibrate(6);
    // 选中格交给下面的 render：它自己也调 markNav。在这里先叫一次，同一个 tick 里
    // render 又会把 data-pop 摘掉，图标那一下弹动就白设了。
    history.pushState(null, "", routeHash(route));
    try {
      await render({ quiet: true, keepScroll: keep, enter: false });
      // 整页换掉之后焦点跟着页走：读屏从新页标题开始读，和点导航进来一样。
      $(".page-title")?.focus({ preventScroll: true });
    } finally {
      swipeCleanup();
      swipe.settling = false;
    }
  }

  async function swipeRevert(d) {
    const gen = ++swipe.gen;
    swipe.settling = "back";
    // 页面与胶囊一起往回走：等页面回完了再让底栏动，会慢半拍。
    await swipeSettle(d, 0, d.release, gen);
    if (gen !== swipe.gen) return;
    swipeCleanup();
    swipe.settling = false;
    paintNavPill(true, true);
  }

  function bindSwipe() {
    document.addEventListener("pointerdown", (event) => {
      // 第二根手指按下来：这是缩放，不是翻页，把刚才那一下退回去让给浏览器。
      if (swipe.drag && event.pointerId !== swipe.drag.id) {
        const d = swipe.drag;
        swipe.drag = null;
        if (d.locked) void swipeRevert(d);
        return;
      }
      if (event.pointerType === "mouse" || event.isPrimary === false) return;
      if (!swipeAvailable() || !event.target.closest?.("#view")) return;
      if (event.target.closest(SWIPE_SKIP)) return;
      swipe.drag = {
        id: event.pointerId,
        x0: event.clientX,
        y0: event.clientY,
        lastX: event.clientX,
        lastT: performance.now(),
        velocity: 0,
        offset: 0,
        painted: 0,
        locked: false,
        step: 0,
        target: null,
        keep: 0,
        view: $("#view"),
      };
    }, { passive: true });

    document.addEventListener("pointermove", (event) => {
      const d = swipe.drag;
      if (!d || event.pointerId !== d.id) return;
      const dx = event.clientX - d.x0;
      const dy = event.clientY - d.y0;
      if (!d.locked) {
        // 明显是在上下滚：把这次手势还给浏览器，别拦着人读页面。
        if (Math.abs(dy) > SWIPE_GATE && Math.abs(dy) > Math.abs(dx) * SWIPE_TILT) {
          swipe.drag = null;
          return;
        }
        if (Math.abs(dx) < SWIPE_GATE) return;
        swipeLock(d, dx);
      }
      const now = performance.now();
      const dt = Math.max(1, now - d.lastT);
      // 速度取短时平均：手指一顿，落格的判断就跟着变温柔。
      d.velocity = d.velocity * 0.65 + ((event.clientX - d.lastX) / dt) * 0.35;
      d.lastX = event.clientX;
      d.lastT = now;
      d.offset = dx;
      // 一帧一次：指针事件的采样常比屏幕刷新密（部分安卓机到 240Hz），逐条重排只会白烧帧。
      if (!swipe.frame) {
        swipe.frame = requestAnimationFrame(() => {
          swipe.frame = 0;
          swipePaintFrame();
        });
      }
    }, { passive: true });

    document.addEventListener("pointerup", (event) => {
      const d = swipe.drag;
      if (!d || event.pointerId !== d.id) return;
      // 先把上一帧补上，弹簧要接着手指最后停的地方起步，不能从半帧前的位置起步。
      if (swipe.frame) {
        cancelAnimationFrame(swipe.frame);
        swipe.frame = 0;
        swipePaintFrame();
      }
      swipe.drag = null;
      if (!d.locked) return;
      swipe.suppressClickUntil = performance.now() + 250;
      const width = viewportW();
      // 松手那一刻的速度（px/s）原样交给弹簧：甩出去就顺着劲走完，慢慢推就慢慢收回。
      d.release = Math.abs(d.offset) > 1 ? d.velocity * 1000 : 0;
      const toward = Math.sign(d.offset) * d.release;
      const far = Math.abs(d.offset) > Math.min(width * 0.3, 200);
      const flick = toward > 320;
      const go = Boolean(d.target) && Math.abs(d.offset) > 16 && (far || flick);
      if (go && hasFormDrafts()) {
        snackbar("这一页还有没保存的修改，先保存或放弃再换页", "warning");
        void swipeRevert(d);
        return;
      }
      if (go) void swipeCommit(d);
      else void swipeRevert(d);
    }, { passive: true });

    const abort = () => {
      const d = swipe.drag;
      if (!d?.locked) {
        swipe.drag = null;
        return;
      }
      swipe.drag = null;
      void swipeRevert(d);
    };
    document.addEventListener("pointercancel", abort, { passive: true });
    addEventListener("blur", abort);

    // 滑完手指抬起时浏览器还会补一个 click：吃掉它，别顺手点开了卡片。
    document.addEventListener("click", (event) => {
      if (performance.now() < swipe.suppressClickUntil) {
        event.preventDefault();
        event.stopPropagation();
        swipe.suppressClickUntil = 0;
      }
    }, true);
  }

  function parseRoute() {
    const [head = "", ...rest] = location.hash.replace(/^#\/?/, "").split("/");
    // 旧书签 / 旧链接：#/ambient 现在就是 ambient 插件的详情。
    if (head === "ambient") return { page: "plugins", arg: "ambient", key: "plugins/ambient" };
    const page = PAGES.some((p) => p.id === head) ? head : "overview";
    let arg = null;
    if (page === "settings") {
      const arg = ["appearance", "connections", "system"].includes(rest[0]) ? rest[0] : null;
      return { page, arg, key: arg ? `settings/${arg}` : page };
    }
    if (page === "plugins" && rest.length) {
      try {
        arg = decodeURIComponent(rest.join("/")) || null;
      } catch {
        arg = null;
      }
    }
    return { page, arg, key: page === "plugins" && arg ? `plugins/${arg}` : page };
  }

  function setTitle(title) {
    document.title = title ? `${title} · ${NAME}` : NAME;
    $("#topbar-title").textContent = title || "";
  }

  let progressTimer = 0;
  function busy(on) {
    clearTimeout(progressTimer);
    const view = $("#view");
    if (on) {
      view.setAttribute("aria-busy", "true");
      progressTimer = setTimeout(() => ($("#progress").hidden = false), 150);
    } else {
      view.removeAttribute("aria-busy");
      $("#progress").hidden = true;
    }
  }

  let titleObserver = null;
  /** 页头 h1 滚出顶栏之后，顶栏接过页名并换容器色（HIG 大标题收拢 + M3 滚动态）。 */
  function watchTitle() {
    titleObserver?.disconnect();
    const bar = $("#topbar");
    const heading = $(".page-title");
    bar.removeAttribute("data-scrolled");
    if (!heading || !("IntersectionObserver" in window)) return;
    titleObserver = new IntersectionObserver(
      ([entry]) => bar.toggleAttribute("data-scrolled", !entry.isIntersecting && entry.boundingClientRect.top < 0),
      { rootMargin: `-${bar.offsetHeight}px 0px 0px 0px` }
    );
    titleObserver.observe(heading);
  }

  const PAINT = {
    overview: () => overviewPage(),
    plugins: (route) => pluginsPage(route),
    logs: () => logsPage(),
    settings: (route) => (route.arg === "appearance" ? appearancePage() : settingsPage(route)),
  };

  const MOUNT = {
    overview: () => overviewFeed.start(),
    logs: () => logs.mount(),
    settings: () => {
      appearance.mount();
      syncGlobalDraft();
    },
  };


  /** 用户亲手输入过的文本框。真实输入（键入、粘贴、输入法、拖放、撤销）会发 `beforeinput`，
   *  随后的 `input` 也带非空的 `inputType`；浏览器与密码管理器的自动填充、扩展写值、表单恢复
   *  两样都没有——这些值再不同于基线，也不是「用户没保存的编辑」，更不该拿来拦人换页。 */
  const typed = new WeakSet();
  const markTyped = (event) => {
    const target = event.target;
    const real = event.type === "beforeinput" || (typeof event.inputType === "string" && event.inputType !== "");
    if (real && event.isTrusted && (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement)) typed.add(target);
  };

  /** 访问令牌框：浏览器按本站（口令框）存过的登录自动填进来的只可能是控制台口令，绝不是实现端令牌。 */
  const tokenField = (form) => $('input[name="access_token"]', form);
  const tokenValue = (form) => {
    const field = tokenField(form);
    return field && typed.has(field) ? field.value : "";
  };
  function scrubAutofill(root) {
    for (const field of $$('input[name="access_token"]', root ?? document)) if (field.value && !typed.has(field)) field.value = "";
  }

  /** 开关现在和打开页面时是否不同。两边都只看是不是 "true"，写成 ""、"false" 或没写都一样。 */
  const switchChanged = (button) =>
    button.dataset.initial !== undefined && (button.dataset.initial === "true") !== (button.getAttribute("aria-checked") === "true");

  /** 表单里有没有用户没保存的改动。`busyCounts` 决定「正在保存」算不算改动：
   *  跨重画保留草稿用默认 true（在途的值也不能被重画冲掉），
   *  离开确认传 false——写入已经在路上，拿它拦人是误报。 */
  function formDirty(form, busyCounts = true) {
    if (form.getAttribute("aria-busy") === "true") return busyCounts;
    const saving = (control) => control.getAttribute("aria-busy") === "true";
    return $$("input, textarea", form).some((input) => (busyCounts || !saving(input)) && typed.has(input) && input.value !== input.defaultValue)
      || $$("[data-form-switch]", form).some(switchChanged)
      || $$("select", form).some((select) => (busyCounts || !saving(select)) && select.value !== (select.defaultValue ?? [...select.options].find((option) => option.defaultSelected)?.value))
      || (busyCounts && !!$("[data-path][aria-busy=true]", form));
  }

  const FORMS = "#global-form, [data-bot], [data-config]";

  /** 带着没保存改动的表单，用人话命名，给离开确认列出来。 */
  function unsavedAreas() {
    return [
      ...$$(FORMS).filter((form) => formDirty(form, false)).map((form) => {
        if (form.id === "global-form") return "全局设置";
        if (form.hasAttribute("data-bot")) return form.dataset.bot === "" ? "新连接" : `连接 ${Number(form.dataset.bot) + 1}`;
        const plugin = plugins.data?.plugins.find((item) => item.name === form.dataset.config);
        return `${plugin?.display || form.dataset.config} 的配置`;
      }),
      ...Object.keys(ambient.drafts).filter((name) => dirty(name)).map((name) =>
        `搭话${SOURCES.find((source) => source.name === name)?.title || name}（没保存）`
      ),
      ...$$("[data-rule][data-unsaved]").map((entry) => `自动回复规则 #${entry.dataset.rule}（没存上）`),
    ];
  }

  /** 离开时要不要拦人。正在保存的控件不算未保存：提交会自己跑完，回执到了基线才更新，
   *  否则「点保存→立刻换页」会弹一个假的「离开未保存的编辑」。 */
  const hasFormDrafts = () =>
    $$(FORMS).some((form) => formDirty(form, false))
    || Object.keys(ambient.drafts).some((name) => dirty(name))
    || !!$("[data-rule][data-unsaved]");

  /* 历史条目的单调编号：只用来判断「这一步是前进还是后退」，取消离开时好原路退回，
     而不是 replaceState 把后退栈改写成两个同样的条目（安卓手势返回会因此「按一下没反应」）。 */
  let entryClock = 0;
  let undoing = 0;
  const routeHash = (route) => `#/${route.page}${route.arg ? "/" + encodeURIComponent(route.arg) : ""}`;

  function claimEntry(route) {
    if (history.state?.zw === undefined) {
      entryClock = Math.max(entryClock + 1, Date.now());
      history.replaceState({ zw: entryClock }, "");
    }
    route.entry = history.state.zw;
  }

  /** 地址栏和屏幕上的页对不上时，以屏幕上的为准。 */
  function settleUrl() {
    clearTimeout(undoing);
    undoing = 0;
    if (shown && parseRoute().key !== shown.key) history.replaceState(history.state, "", routeHash(shown));
  }

  function undoNavigation(previous) {
    const arrived = history.state?.zw;
    const forward = arrived === undefined || arrived > (previous.entry ?? 0);
    undoing = setTimeout(settleUrl, 500);
    history.go(forward ? -1 : 1);
  }

  let renderSeq = 0;
  let shown = null;
  const scrolls = new Map();

  /** 画当前路由。`quiet` 用于刷新：不出骨架、不挪焦点、保留滚动位置。
   *  `keepScroll` / `enter` 只给手势翻页用：落页时按目标页上次的位置停下，
   *  并且不再播一次「整页升起」——那一段在滑动里已经走完了。 */
  async function render({ quiet = false, keepScroll = null, enter = true } = {}) {
    if (!token) {
      lock();
      return;
    }
    const route = parseRoute();
    // 旧的 #/ambient 换成插件详情自己的地址，刷新、复制与后退都落在同一页上。
    if (/^#\/ambient\/?(?:[?#].*)?$/.test(location.hash)) history.replaceState(history.state, "", routeHash(route));
    const previous = shown;
    if (previous && previous.key !== route.key) {
      const unsaved = unsavedAreas();
      if (unsaved.length) {
        const names = unsaved.length > 3 ? `${unsaved.slice(0, 3).map((name) => `「${name}」`).join("")}等 ${unsaved.length} 处` : unsaved.map((name) => `「${name}」`).join("");
        const leave = await confirmAction({
          title: "还有没保存的修改",
          body: `${names}的修改还没有保存，现在离开会丢掉它们。`,
          confirm: "放弃修改并离开",
          cancel: "继续编辑",
          danger: true,
        });
        if (!leave) {
          undoNavigation(previous);
          return;
        }
      }
    }
    const changed = !previous || previous.key !== route.key;
    // 停着的那一屏：源站已经不在了，换哪一页都是同一张静态停机页。
    if (lifecycle.stopped) {
      if (route.page !== "overview") history.replaceState(null, "", "#/");
      shown = { page: "overview", arg: null, key: "overview" };
      markNav("overview");
      setTitle("已停止");
      busy(false);
      put($("#view"), lifecycle.stoppedPage());
      watchTitle();
      if (!quiet) $(".page-title")?.focus({ preventScroll: true });
      return;
    }
    markNav(route.page);
    overviewFeed.stop();
    if (route.page !== "logs") logs.stop();

    // 宽屏插件页：列表已经在屏上，只换右侧详情，搜索词、焦点与滚动都不动。
    if (!quiet && previous?.page === "plugins" && route.page === "plugins" && wide.matches && $("#plugin-detail")) {
      shown = route;
      claimEntry(route);
      await plugins.swapDetail(route.arg);
      return;
    }

    const seq = ++renderSeq;
    if (previous && changed) scrolls.set(previous.key, scrollY);
    if (changed) setTitle(PAGES.find((p) => p.id === route.page)?.label);
    busy(true);
    if (changed && !quiet) put($("#view"), skeleton());

    let fragment;
    try {
      fragment = await PAINT[route.page](route);
      lifecycle.online();
    } catch (error) {
      if (seq !== renderSeq || error?.status === 401) return;
      if (error?.offline && lifecycle.panel) {
        busy(false);
        shown = route;
        claimEntry(route);
        put($("#view"), await lifecycle.page());
        $("#view > .page")?.toggleAttribute("data-enter", changed);
        setTitle("已停止");
        watchTitle();
        lifecycle.watch();
        if (changed && previous && !quiet) $(".page-title")?.focus({ preventScroll: true });
        return;
      }
      busy(false);
      shown = route;
      claimEntry(route);
      put(
        $("#view"),
        h`<div class="page">${pageHead("没能打开这一页")}
          <div class="card"><div class="empty" role="alert">
            <span class="empty-mark">${icon("error")}</span><p>${error?.message || "原因不明"}</p>
            <button class="btn btn-tonal" type="button" data-retry>${icon("refresh")}重试</button>
          </div></div></div>`
      );
      watchTitle();
      return;
    }
    if (seq !== renderSeq) return;

    const oldFocus = document.activeElement;
    const focusPath = oldFocus?.dataset?.path;
    const focusId = oldFocus?.id;
    const retainedForms = previous?.key === route.key
      ? $$("#global-form, [data-bot], [data-config]").filter(form => form.dataset.refresh !== "replace" && formDirty(form)) : [];
    const keep = keepScroll ?? (quiet ? scrollY : changed ? scrolls.get(route.key) || 0 : scrollY);
    shown = route;
    claimEntry(route);
    busy(false);
    put($("#view"), fragment);
    scrubAutofill();
    for (const form of retainedForms) {
      const selector = form.id ? `#${CSS.escape(form.id)}` : form.hasAttribute("data-bot")
        ? `[data-bot="${CSS.escape(form.dataset.bot)}"]` : `[data-config="${CSS.escape(form.dataset.config)}"]`;
      const fresh = $(selector);
      if (fresh) fresh.replaceWith(form);
      else if (form.dataset.bot === "") $("#bot-list")?.append(form);
    }
    const page = $("#view > .page");
    if (changed && enter && page) page.toggleAttribute("data-enter", true);
    const heading = $(".page-title");
    if (heading) setTitle(page?.dataset.title || heading.textContent.trim());
    scrollTo({ top: keep, behavior: "instant" });
    watchTitle();
    MOUNT[route.page]?.();
    for (const button of $$("[data-form-switch]")) button.dataset.initial ??= button.getAttribute("aria-checked");
    // 换页后把焦点交给新页的标题：读屏从这里开始读，键盘从这里继续走。
    if (changed && previous && !quiet) heading?.focus({ preventScroll: true });
    else if (quiet && oldFocus && oldFocus !== document.body) {
      const next = oldFocus.isConnected ? oldFocus : focusPath
        ? $(`[data-path="${CSS.escape(focusPath)}"]`) : focusId ? document.getElementById(focusId) : null;
      (next || heading)?.focus({ preventScroll: true });
    }
  }

  /* ==================== §7 总览 ==================== */

  /** 总览读到的起停能力：能不能停、管理面板在不在。只在「装好的那一份」上为真。 */
  const powerInfo = { available: false, panel: null, version: "" };

  const overviewFeed = {
    timer: 0,
    base: 0,
    at: 0,
    start() {
      this.stop();
      const hero = $("[data-uptime]");
      if (hero) {
        this.base = Number(hero.dataset.uptime) || 0;
        this.at = Date.now();
      }
      this.tick();
      this.timer = setInterval(() => this.tick(), 6000);
    },
    stop() {
      clearInterval(this.timer);
      this.timer = 0;
    },
    async tick() {
      const box = $("#overview-log");
      if (!box || document.hidden) return this.stop();
      const hero = $("[data-uptime]");
      if (hero) hero.textContent = duration(this.base + (Date.now() - this.at) / 1000);
      try {
        const data = await api("/logs?limit=4");
        const html = part((data.lines || []).slice(-4).map(logRow)) || part(h`<p class="log-empty">还没有日志</p>`);
        if (box.dataset.digest !== html) {
          box.dataset.digest = html;
          box.innerHTML = html;
        }
      } catch {
        if (!box.children.length) put(box, h`<p class="log-empty">暂时读不到日志</p>`);
      }
    },
  };

  /** 平台名说成人话：实现端报的是协议里的平台码（red 是 QQ）。 */
  const platformName = (platform) =>
    PLATFORM_NAMES[String(platform || "").toLowerCase()] || platform || "未知平台";

  /** 头像或首字：微信实现端的登录快照不带头像，退成名字的第一个字。 */
  const avatarInitial = (bot) => {
    const text = [bot.name, bot.nick, bot.id].find((value) => typeof value === "string" && value.trim());
    return text ? [...text.trim()][0] : "?";
  };
  const botAvatar = (bot) => {
    const initial = avatarInitial(bot);
    return bot.avatar
      ? h`<span class="tile tile-avatar" aria-hidden="true"><img class="avatar" src="${bot.avatar}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer" data-initial="${initial}"></span>`
      : h`<span class="tile tile-avatar" aria-hidden="true"><span class="avatar-initial">${initial}</span></span>`;
  };

  async function overviewPage() {
    const data = await api("/overview");
    powerInfo.available = Boolean(data.power?.available);
    powerInfo.panel = data.power?.panel ?? null;
    powerInfo.version = data.app.version;
    const health = !data.bots.length
      ? { label: "等待连接", title: "还差一步就能开始", note: "添加实现端连接并重启，知微就能开始处理消息。", tone: "warning" }
      : data.plugins.pending
        ? { label: "需要重启", title: `${data.plugins.pending} 个插件等待重启`, note: "其余服务运行正常，重启后即可完成应用。", tone: "warning" }
        : { label: "运行良好", title: "一切正常", note: `${data.bots.length} 个账号在线，${data.plugins.on} 个插件已就绪。`, tone: "success" };
    // 只有要处理的时候才出现一枚实心按钮；平常的重启 / 停止退成描边，焦点留给状态本身。
    const primary = !data.bots.length
      ? h`<a class="btn btn-filled" href="#/settings/connections">${icon("add")}添加连接</a>`
      : "";
    /* 读数带只有三格：标签在上、数字在下，注脚留在宽屏（窄屏挤成两行反而更乱）。
       连续运行挪进了页头的副标题——它是「什么时候起的」，不是这一页要回答的事。 */
    const metric = (label, value, unit, note, attrs = "") =>
      h`<div class="metric">
        <dt class="metric-label">${label}</dt>
        <dd class="metric-value"><span class="num"${raw(attrs)}>${value}</span>${unit ? h`<span class="metric-unit">${unit}</span>` : ""}</dd>
        <dd class="metric-note">${note}</dd>
      </div>`;
    const bots = data.bots.length
      ? h`<ul class="stage-bots" aria-label="在线账号">${data.bots.map(
          (bot) => h`<li class="stage-bot">
            ${botAvatar(bot)}
            <span class="stage-bot-text">
              <span class="stage-bot-name">${bot.name || bot.nick || "未识别的账号"}</span>
              <span class="stage-bot-sub"><span class="stage-bot-state">在线</span> · ${platformName(bot.platform)}${bot.id ? ` ${bot.id}` : " · 等待账号信息"}</span>
            </span>
          </li>`
        )}</ul>`
      : h`<p class="stage-empty">${icon("hub")}<span>还没有在线账号。添加连接并重启后，就能在这里看到状态。</span></p>`;

    return h`<div class="page home-page">
      <header class="page-head">
        <div class="page-head-text">
          <div class="home-lead">
            <h1 class="page-title" tabindex="-1">总览</h1>
            <span class="page-version">v${data.app.version}</span>
          </div>
          <p class="page-sub">启动于 <time>${data.app.started}</time> · 已连续运行 <span class="page-uptime" data-uptime="${data.app.uptime}">${duration(data.app.uptime)}</span></p>
        </div>
      </header>
      <div class="overview">
        <section class="hero stage" aria-labelledby="hero-label" data-started="${data.app.started}" data-tone="${health.tone}">
          <div class="stage-head">
            <div class="stage-copy">
              <span class="status status-${health.tone}">${health.label}</span>
              <h2 class="stage-title" id="hero-label">${health.title}</h2>
              <p class="stage-note">${health.note}</p>
            </div>
            <div class="actions stage-actions" role="group" aria-label="运行">
              ${primary}
              <button class="btn ${data.plugins.pending ? "btn-filled" : "btn-tonal"}" type="button" data-restart>${icon("restart_alt")}重启</button>
              ${powerInfo.available ? h`<button class="btn btn-text btn-danger btn-stop" type="button" data-lifecycle="stop">${icon("stop_circle")}停止</button>` : ""}
            </div>
          </div>
          <dl class="stage-metrics">
            ${metric("今日消息", num(data.messages.today), "条", `近 7 天 ${num(data.messages.week)} 条`)}
            ${metric("今日发言", num(data.messages.people), "人", "去重后的发言人")}
            ${metric("插件", num(data.plugins.on), `/ ${data.plugins.total}`, data.plugins.pending ? `${data.plugins.pending} 个待重启` : "按当前配置运行")}
          </dl>
          <div class="stage-foot">
            ${bots}
            <a class="btn btn-text stage-link" href="#/settings/connections">管理连接${icon("chevron_right", "i-end")}</a>
          </div>
        </section>

        <section class="section home-activity" aria-labelledby="recent-title">
          <div class="section-head">
            <h2 class="section-title" id="recent-title">实时动态</h2>
            <a class="btn btn-text" href="#/logs">全部日志${icon("chevron_right", "i-end")}</a>
          </div>
          <div class="log log-short home-log" id="overview-log" aria-live="off"><p class="log-empty">正在读取最近日志…</p></div>
        </section>
      </div>
    </div>`;
  }

  /**
   * 重启。回执只说明「已经排上」：真正的就绪以新进程的启动时间为准，
   * 所以轮询到 `app.started` 变了才刷新，而不是看到端口能连上就当成功——
   * 旧进程在收尾的那一小段里还答得出来。
   */
  async function restartBot(button) {
    const started = $(".hero")?.dataset.started ?? "";
    const ok = await confirmAction({
      title: "重启知微？",
      body: "会重新载入配置与插件；正在进行的对话、截图与排期随之中断，几秒后本页自动恢复。",
      confirm: "重启",
    });
    if (!ok) return;

    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    busy(true);
    try {
      const reply = await api("/restart", { method: "POST" });
      snackbar(reply.message);
      holdSnack(true);
    } catch (error) {
      button.disabled = false;
      button.removeAttribute("aria-busy");
      busy(false);
      report(error);
      return;
    }

    if (await waitBack(started)) return;
    holdSnack(false);
    busy(false);
    button.disabled = false;
    button.removeAttribute("aria-busy");
    snackbar("重启还没有回来，稍后刷新页面看看。", "error");
  }

  /** 等新进程回来：启动时间变了才算（旧进程收尾的那一小段里还答得出来）。回来了就整页刷新。 */
  async function waitBack(started, rounds = 90) {
    // 先把回执那一小段等过去，再开始探活。
    await new Promise((done) => setTimeout(done, 1800));
    for (let i = 0; i < rounds; i++) {
      try {
        const data = await api("/overview");
        if (data.app.started !== started) {
          location.reload();
          return true;
        }
      } catch {
        /* 端口关着就是还在重启 */
      }
      await new Promise((done) => setTimeout(done, 1200));
    }
    return false;
  }

  /* ==================== §7b 新版本 ==================== */

  /**
   * 有新版本时顶栏出现一枚「更新」药丸（每一页都看得见，所以只放这一处）；没有就什么都不显示。
   * 知微在跑时由它自己下载、换上、重启（/api/update）；停着时交给管理面板（/panel/update）。
   */
  const updates = {
    info: null,
    checked: 0,
    async check(force = false) {
      if (!force && Date.now() - this.checked < 30 * 60 * 1000) return this.paint();
      try {
        if (lifecycle.offline) {
          const status = await lifecycle.refresh();
          this.info = { current: status.version, managed: status.managed, latest: status.latest };
        } else {
          this.info = await api(`/update${force ? "?force=true" : ""}`);
        }
        this.checked = Date.now();
      } catch {
        /* 查不到就当没有，不打扰 */
      }
      this.paint();
    },
    latest() {
      return this.info?.managed ? this.info.latest : null;
    },
    paint() {
      const slot = $("#update-slot");
      if (!slot) return;
      const latest = this.latest();
      const html = latest
        ? h`<button class="update-chip state" type="button" data-update aria-label="更新到 ${latest.version}" title="有新版本 ${latest.version}">
            ${icon("system_update_alt")}<span class="update-chip-label">新版本</span></button>`
        : "";
      const digest = latest?.version ?? "";
      if (slot.dataset.digest === digest) return;
      slot.dataset.digest = digest;
      put(slot, html);
    },
    async apply(button) {
      const latest = this.latest();
      if (!latest) return;
      const notes = String(latest.notes || "").replace(/^#.*\n+/, "").trim();
      const ok = await confirmAction({
        title: `更新到 ${latest.version}？`,
        body: lifecycle.offline
          ? "下载、校验后换上新版本；知微现在停着，换好之后照旧停着，点「启动」即可用上。"
          : "下载、校验后换上新版本并自动重启；正在进行的对话与截图会中断，本页随后自动恢复。",
        detail: notes.length > 1200 ? `${notes.slice(0, 1200)}…` : notes,
        confirm: "更新",
      });
      if (!ok) return;
      const buttons = $$("[data-update]");
      for (const item of buttons) {
        item.disabled = true;
        item.setAttribute("aria-busy", "true");
      }
      if (lifecycle.offline) return lifecycle.run("update", button);
      const started = $(".hero")?.dataset.started ?? "";
      busy(true);
      snackbar(`正在下载 ${latest.version} …`);
      holdSnack(true);
      try {
        const reply = await api("/update", { method: "POST" });
        snackbar(reply.message);
        holdSnack(true);
      } catch (error) {
        holdSnack(false);
        busy(false);
        for (const item of buttons) {
          item.disabled = false;
          item.removeAttribute("aria-busy");
        }
        report(error);
        return;
      }
      if (await waitBack(started, 120)) return;
      holdSnack(false);
      busy(false);
      snackbar("新版本已换上，但还没等到重启回来；稍后刷新页面看看。", "error");
    },
  };

  /* ==================== §7c 管理面板 ==================== */

  /** 终端日志一行 → 与控制台日志同一种行（`[时:分:秒] [级别] [来源] 正文`）。 */
  function terminalRow(line) {
    const match = /^\[(\d\d:\d\d:\d\d)\]\s*\[(\w+)\]\s*\[([^\]]*)\]\s?(.*)$/.exec(line);
    return match
      ? logRow({ at: match[1], level: match[2], target: match[3], text: match[4] })
      : logRow({ at: "", level: "INFO", target: "", text: line });
  }

  const JOB_NAMES = { start: "启动", stop: "停止", halt: "全面停止", restart: "重启", update: "更新" };

  /**
   * 经管理面板（acumen --panel）打开时才有的一层：知微停着时的「已停止」页、启动 / 停止、
   * 停机状态下的更新。直接打开控制台时 `panel` 为假，这一层整个不出现。
   */
  const lifecycle = {
    panel: false,
    offline: false,
    /** 本机停机页的模式：停到哪一步、面板还在不在。非空时所有页面都换成那一屏。 */
    stopped: null,
    status: null,
    /** 停机页上那一次画的是「没在运行」还是「没有回应」、按钮是不是被作业按住了。 */
    runningShown: false,
    jobShown: false,
    timer: 0,
    async probe() {
      try {
        this.status = await panelApi("/status");
        this.panel = true;
      } catch {
        this.panel = false;
      }
    },
    async refresh() {
      this.status = await panelApi("/status");
      return this.status;
    },
    online() {
      if (!this.offline) return;
      this.offline = false;
      this.stop();
      updates.check(true);
    },
    stop() {
      clearTimeout(this.timer);
      this.timer = 0;
    },
    /** 停机页上每几秒看一眼：命令跑完了、知微起来了，就换回正常页面。 */
    watch() {
      this.offline = true;
      this.stop();
      const tick = async () => {
        if (!this.offline || document.hidden) return;
        let status;
        try {
          status = await this.refresh();
        } catch {
          this.timer = setTimeout(tick, 4000);
          return;
        }
        const box = $("#job-box");
        if (box) put(box, this.jobBlock(status.job));
        updates.info = { current: status.version, managed: status.managed, latest: status.latest };
        updates.paint();
        if (status.running && !status.job?.running) {
          try {
            await api("/overview");
            this.online();
            render({ quiet: true });
            return;
          } catch {
            /* 进程在、控制台还没起来 */
          }
        }
        // 刚停下的那几秒里进程还在收尾、命令也还没退：这一屏要从「没有回应 / 重启」换成
        // 「没在运行 / 启动」，按住的那枚按钮也要松开。
        const jobRunning = Boolean(status.job?.running);
        if (status.running !== this.runningShown || jobRunning !== this.jobShown) {
          this.runningShown = status.running;
          this.jobShown = jobRunning;
          this.status = status;
          render({ quiet: true });
          return;
        }
        this.timer = setTimeout(tick, status.job?.running ? 1500 : 4000);
      };
      this.timer = setTimeout(tick, 1500);
    },
    jobBlock(job) {
      if (!job) return "";
      const name = JOB_NAMES[job.command] || job.command;
      if (job.running) {
        return h`<div class="job" role="status">
          <span class="job-title">正在${name}…</span>
          <div class="progress" aria-hidden="true"></div>
          ${job.output ? h`<pre class="job-output">${job.output}</pre>` : ""}
        </div>`;
      }
      if (job.code && Date.now() / 1000 - job.started < 600) {
        return h`<div class="job" role="alert">
          <span class="job-title">${icon("error-fill")}${name}没有成功</span>
          ${job.output ? h`<pre class="job-output">${job.output}</pre>` : ""}
        </div>`;
      }
      return "";
    },
    async page() {
      const status = this.status ?? (await this.refresh());
      this.runningShown = status.running;
      updates.info = { current: status.version, managed: status.managed, latest: status.latest };
      updates.paint();
      const log = await panelApi("/log?lines=40").catch(() => ({ lines: [] }));
      const busyJob = status.job?.running;
      this.runningShown = status.running;
      this.jobShown = Boolean(busyJob);
      const reason = status.stopped
        ? "你上次让它停下了；开机与守护都不会自己拉起它，点「启动」就好。"
        : status.running
          ? "进程在，但控制台还没有回应：可能正在启动，或者卡住了。看看下面的终端日志。"
          : "进程没在跑：可能刚崩溃，runit 会自己再拉起；一直起不来就看看下面的终端日志。";
      return h`<div class="page" data-title="已停止">
        ${pageHead("总览", "这台设备上知微的运行情况。")}
        <div class="overview">
          <section class="hero" data-offline aria-labelledby="hero-label">
            <div class="hero-top"><span>${NAME} v${status.version}</span><span class="status status-idle">${status.running ? "无响应" : "已停止"}</span></div>
            <h2 class="hero-label" id="hero-label">知微现在</h2>
            <p class="hero-value">${status.running ? "没有回应" : "没在运行"}</p>
            <p class="hero-note">${reason}</p>
            <div class="actions">
              <button class="btn btn-filled" type="button" data-lifecycle="${status.running ? "restart" : "start"}"${attr(busyJob, "disabled")}>
                ${icon(status.running ? "restart_alt" : "power_settings_new")}${status.running ? "重启" : "启动"}</button>
            </div>
          </section>
          <div id="job-box">${this.jobBlock(status.job)}</div>
          <section class="section section-wide" aria-labelledby="terminal-title">
            <div class="section-head">
              <h2 class="section-title" id="terminal-title">终端日志</h2>
              <span class="section-meta">最后 ${log.lines?.length || 0} 行</span>
            </div>
            <div class="log log-short">${log.lines?.length ? (log.lines || []).map(terminalRow) : h`<p class="log-empty">还没有日志</p>`}</div>
          </section>
        </div>
      </div>`;
    },
    /** 交管理命令做一件事；停机页随后轮询它的进度。 */
    async run(command, button) {
      if (command === "stop") return this.shutdown(button);
      if (button) {
        button.disabled = true;
        button.setAttribute("aria-busy", "true");
      }
      try {
        const reply = await panelApi(`/${command}`, { method: "POST" });
        snackbar(reply.message);
      } catch (error) {
        if (button) {
          button.disabled = false;
          button.removeAttribute("aria-busy");
        }
        report(error);
        return;
      }
      this.status = null;
      this.offline = true;
      if (shown) render({ quiet: true });
    },

    /**
     * 总览上那颗「停止」。先问一句停到哪一步，再把话交给管理命令：
     * `bot` 只停机器人，`all` 连管理面板与开机守护一起停。
     *
     * 停下之后分两种去向：面板页且只停机器人 → 面板上的停机页（有启动与终端日志）；
     * 其余情况 → 本机的一张静态停机页，因为这一页的源站已经不在了。
     */
    async shutdown(button) {
      const scope = await stopDialog();
      if (!scope) return;
      const all = scope === "all";
      const panel = this.panel ? { ready: true, url: null } : powerInfo.panel;
      if (button) {
        button.disabled = true;
        button.setAttribute("aria-busy", "true");
      }
      busy(true);
      try {
        const reply = this.panel
          ? await panelApi(all ? "/halt" : "/stop", { method: "POST" })
          : await api("/power", { method: "POST", body: { scope } });
        snackbar(reply.message);
        holdSnack(true);
      } catch (error) {
        if (button) {
          button.disabled = false;
          button.removeAttribute("aria-busy");
        }
        busy(false);
        report(error);
        return;
      }
      const gone = await this.waitDown();
      busy(false);
      holdSnack(false);
      if (!gone) {
        // 命令没把进程停掉（配置写坏了、管理命令报错）：留在这张页面上，说清看到的事。
        if (button) {
          button.disabled = false;
          button.removeAttribute("aria-busy");
        }
        snackbar("停止命令还没有结果，稍后刷新看看。", "error");
        return;
      }
      if (this.panel && !all) {
        dismissSnack();
        this.status = null;
        this.offline = true;
        if (shown) render({ quiet: true });
        return;
      }
      this.stopped = {
        scope,
        panel: all ? null : panel,
        version: this.status?.version || powerInfo.version || "",
      };
      dismissSnack();
      // 服务已经停在下面了：顶栏那枚「有新版本」这时也点不动了。
      updates.info = null;
      updates.paint();
      history.replaceState(null, "", "#/");
      if (shown) render({ quiet: true });
      this.watchLocal();
    },

    /** 等进程真的停下：控制台不再应答（面板转发的 503 也算）。最多约 17 秒。 */
    async waitDown() {
      for (let i = 0; i < 24; i++) {
        await new Promise((done) => setTimeout(done, 700));
        try {
          await api("/overview");
        } catch (error) {
          if (error?.offline || !error?.status) return true;
        }
      }
      return false;
    },

    /** 本机的一张静态停机页：服务已经不在了，这一屏不靠任何接口。 */
    stoppedPage() {
      const stopped = this.stopped || {};
      const panel = stopped.panel;
      const all = stopped.scope === "all";
      const note = all
        ? "机器人、管理面板与开机守护都已停下。在 Termux 里运行 acumen start 重新开始。"
        : panel?.ready
          ? "管理面板还在运行，可以从那里再启动它；终端里运行 acumen start 也一样。"
          : "这台设备的管理面板没在运行。在 Termux 里运行 acumen start 重新开始。";
      return h`<div class="page home-page" data-title="已停止">
        ${pageHead("总览", "这台设备上知微的运行情况。")}
        <div class="overview">
          <section class="hero" data-offline aria-labelledby="hero-label">
            <div class="hero-top"><span>${NAME}${stopped.version ? ` v${stopped.version}` : ""}</span><span class="status status-idle">已停止</span></div>
            <h2 class="hero-label" id="hero-label">知微现在</h2>
            <p class="hero-value">没在运行</p>
            <p class="hero-note">${note}</p>
            <div class="actions">
              ${panel?.url ? h`<a class="btn btn-filled" href="${panel.url}">${icon("arrow_forward")}打开管理面板</a>` : ""}
              <button class="btn btn-outlined" type="button" data-copy-start>${icon("content_copy")}复制启动命令</button>
            </div>
            <p class="hero-foot">知微回来后，这一页会自己刷新。</p>
          </section>
        </div>
      </div>`;
    },

    /** 停着的时候隔几秒敲一下这一页的源站：回来了就整页刷新（不必知道口令）。 */
    watchLocal() {
      this.stop();
      const tick = async () => {
        if (!this.stopped) return;
        if (document.hidden) {
          this.timer = setTimeout(tick, 4000);
          return;
        }
        try {
          await fetch(`${location.origin}/manifest.webmanifest`, { mode: "no-cors", cache: "no-store" });
          location.reload();
          return;
        } catch {
          /* 源站还没回来 */
        }
        this.timer = setTimeout(tick, 4000);
      };
      this.timer = setTimeout(tick, 4000);
    },
  };

  /* ==================== §8 插件 ==================== */

  const plugins = {
    data: null,
    query: "",
    section: "",
    status: "",
    selected: null,
    detailSeq: 0,

    visible() {
      const text = this.query.trim().toLowerCase();
      const squashed = text.replace(/[\s·・/]+/g, "");
      const normalize = (value) => String(value ?? "").toLowerCase().replace(/[\s·・/]+/g, "");
      return this.data.plugins.filter((plugin) => {
        if (this.section && plugin.section !== this.section) return false;
        if (this.status && (this.status === "on") !== !!plugin.on) return false;
        if (!text) return true;
        const section = this.data.sections.find((item) => item.code === plugin.section);
        const meta = sectionMeta(plugin.section);
        // 列表接口带的是完整指令；万一与旧版后端错配成数字，退化成不搜指令，别让整页输不出结果。
        const commands = Array.isArray(plugin.commands) ? plugin.commands : [];
        return [
          plugin.name,
          plugin.display,
          plugin.summary,
          section?.name,
          section?.en,
          meta.name,
          sectionLabel(plugin.section, section?.name || meta.name),
          ...commands.map((command) => `${command.cmd} ${command.note}`),
        ].some((value) => {
          const hay = String(value ?? "").toLowerCase();
          // 「AI资讯」「系统运维」这类去空格、去间隔号的写法也该找得到。
          return hay.includes(text) || (squashed !== "" && normalize(hay).includes(squashed));
        });
      });
    },

    rows() {
      const list = this.visible();
      if (!list.length) return emptyState("没有符合条件的插件。换个关键词或筛选试试。", "search");
      const known = this.data.sections.map((section) => section.code);
      const order = [...known, ...new Set(list.map((plugin) => plugin.section).filter((code) => !known.includes(code)))];
      const nameOf = (code) => this.data.sections.find((section) => section.code === code)?.name || "其他";
      return order
        .map((code) => [code, list.filter((plugin) => plugin.section === code)])
        .filter(([, members]) => members.length)
        .map(([code, members]) => {
          const meta = sectionMeta(code);
          const on = members.filter((plugin) => plugin.on).length;
          return h`<section class="plugin-block" aria-labelledby="block-${code}">
            <h2 class="list-title" id="block-${code}">
              <span class="list-title-text">${nameOf(code)}</span>
              <span class="num" aria-label="共 ${members.length} 个插件，${on} 个已启用">${on < members.length ? `${on} / ${members.length}` : members.length}</span>
            </h2>
            <ul class="group plate-list">${members.map(
              (plugin) => h`<li class="item" data-plugin="${plugin.name}">
                ${pluginTile(plugin)}
                <div class="item-content">
                  <a class="item-link item-title" href="#/plugins/${encodeURIComponent(plugin.name)}"${attr(plugin.name === this.selected, 'aria-current="page"')}>
                    ${plugin.display}${plugin.pending ? h`<span class="status status-warning">待重启</span>` : ""}
                  </a>
                  <span class="item-sub">${plugin.summary}</span>
                </div>
                <div class="item-trailing">${switchButton(plugin, { label: `启用 ${plugin.display}` })}</div>
              </li>`
            )}</ul>
          </section>`;
        });
    },

    /** 只列真的有插件的分类；注册表里新出现、还没登记进分区的代号也要有一格。 */
    categories() {
      const seen = new Set();
      const list = [];
      for (const section of this.data.sections) {
        if (!this.data.plugins.some((plugin) => plugin.section === section.code)) continue;
        seen.add(section.code);
        list.push(section);
      }
      for (const plugin of this.data.plugins) {
        if (seen.has(plugin.section)) continue;
        seen.add(plugin.section);
        list.push({ code: plugin.section, name: sectionMeta(plugin.section).name, en: plugin.section });
      }
      return list;
    },

    /** 筛选芯片：分类与「已停用」铺在同一张等宽网格里，整行对齐、末行补满。
     *  分类图标平时在前面，选中时换成对勾；数量靠右，扫一眼就能比。
     *  读屏是一组开关（aria-pressed），全部都能 Tab 到，方向键只在格子里移动焦点。 */
    filters() {
      const counts = new Map();
      for (const plugin of this.data.plugins) counts.set(plugin.section, (counts.get(plugin.section) || 0) + 1);
      const chip = (code, label, longName, hint, total, glyph) =>
        h`<button class="chip state filter-chip" type="button" data-filter data-section="${code}"
            aria-pressed="${bool(this.section === code)}" aria-controls="plugin-list"
            title="${hint}" aria-label="${longName}：${total} 个插件。${hint}">
          <span class="chip-mark" aria-hidden="true">${icon(glyph, "chip-icon")}${icon("check", "chip-check")}</span>
          <span class="chip-label">${label}</span>
          <span class="chip-count" aria-hidden="true">${total}</span>
        </button>`;
      const off = this.data.plugins.filter((plugin) => !plugin.on).length;
      return [
        chip("", "全部", "全部", "显示所有插件", this.data.plugins.length, "grid_view"),
        ...this.categories().map((section) =>
          chip(section.code, sectionLabel(section.code, section.name), section.name, sectionMeta(section.code).hint,
            counts.get(section.code), sectionMeta(section.code).icon)
        ),
        h`<button class="chip state filter-chip" type="button" data-filter data-status-filter="${this.status === "off" ? "" : "off"}"
            aria-pressed="${bool(this.status === "off")}" aria-controls="plugin-list"
            title="只看已停用的插件" aria-label="只看已停用，${off} 个插件">
          <span class="chip-mark" aria-hidden="true">${icon("power_settings_new", "chip-icon")}${icon("check", "chip-check")}</span>
          <span class="chip-label">已停用</span>
          <span class="chip-count" aria-hidden="true">${off}</span>
        </button>`,
      ];
    },

    filtered() {
      return !!(this.query.trim() || this.section || this.status);
    },

    sectionTotals(code) {
      const members = code ? this.data.plugins.filter((plugin) => plugin.section === code) : this.data.plugins;
      return { total: members.length, on: members.filter((plugin) => plugin.on).length };
    },

    sectionName(code) {
      return this.data.sections.find((section) => section.code === code)?.name || sectionMeta(code).name;
    },

    clearFilters() {
      this.query = "";
      this.section = "";
      this.status = "";
      try {
        localStorage.removeItem("acumen.plugins.section");
      } catch {}
      const input = $("#plugin-search");
      if (input) input.value = "";
      this.repaint();
      input?.focus({ preventScroll: true });
    },

    /** 分类是单选：选中后立即筛列表，并把选择记在本机；焦点留在同一格上。 */
    chooseSection(code) {
      const next = code || "";
      this.section = next;
      try {
        if (next) localStorage.setItem("acumen.plugins.section", next);
        else localStorage.removeItem("acumen.plugins.section");
      } catch {}
      this.repaint();
      const chip = $(`[data-section="${CSS.escape(next)}"]`);
      chip?.focus({ preventScroll: true });
      chip?.scrollIntoView({ block: "nearest", inline: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    },

    /** 页头的一句话：此刻的状态，不是使用说明。 */
    headline() {
      const total = this.data.plugins.length;
      const on = this.data.plugins.filter((plugin) => plugin.on).length;
      const pending = this.data.plugins.filter((plugin) => plugin.pending).length;
      return `${total} 个插件，${on} 个在运行${pending ? `，${pending} 个等待重启` : total > on ? `，${total - on} 个停用` : ""}。`;
    },

    summary() {
      const total = this.data.plugins.length;
      const on = this.data.plugins.filter((plugin) => plugin.on).length;
      const shown = this.visible().length;
      const filtering = !!(this.query.trim() || this.status);
      if (this.section) {
        const stats = this.sectionTotals(this.section);
        const name = this.sectionName(this.section);
        return filtering
          ? `${name}内显示 ${shown} / ${stats.total} 个`
          : `${name} · ${stats.total} 个插件，${stats.on} 个已启用`;
      }
      return filtering ? `显示 ${shown} / ${total} 个` : `共 ${total} 个插件，${on} 个已启用`;
    },

    /** 只重画列表与计数：打一个字就整页重来会把输入法与光标一起冲掉。 */
    repaint() {
      put($("#plugin-list"), this.rows());
      put($("#plugin-filters"), this.filters());
      const head = $("#plugin-head-summary");
      if (head) head.textContent = this.headline();
      const status = $("#plugin-count");
      if (status) status.textContent = this.summary();
      const clear = $("#plugin-clear");
      if (clear) clear.hidden = !this.filtered();
      // 没筛东西时，状态分段已有「全部 / 已启用 / 已停用」三个数，不必再占一行重复。
      const line = $(".plugin-count-line");
      if (line) line.hidden = !this.filtered();
      // 宽屏右栏还没选插件时，占位卡也要跟着当前分类与搜索走，别留着上一分类的推荐。
      const pane = $("#plugin-detail");
      if (pane && !this.selected) put(pane, detailPlaceholder());
    },

    async swapDetail(name) {
      const seq = ++this.detailSeq;
      this.selected = name;
      for (const link of $$("#plugin-list .item-link")) {
        const on = link.closest("[data-plugin]").dataset.plugin === name;
        if (on) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
      }
      const pane = $("#plugin-detail");
      if (!name) {
        put(pane, detailPlaceholder());
        setTitle("插件");
        return;
      }
      pane.setAttribute("aria-busy", "true");
      try {
        const plugin = await api(`/plugins/${encodeURIComponent(name)}`);
        if (seq !== this.detailSeq || !pane.isConnected) return;
        put(pane, detailBody(plugin, 2));
        pane.scrollTop = 0;
        setTitle(plugin.display);
      } catch (error) {
        if (seq !== this.detailSeq || !pane.isConnected) return;
        put(pane, h`<div class="card">${emptyState(error.message, "error")}</div>`);
      } finally {
        if (seq === this.detailSeq) pane.removeAttribute("aria-busy");
      }
    },
  };

  /** 插件开关。名字固定为「启用 某某」，状态交给 aria-checked：开关的名字不随状态变。 */
  const switchButton = (plugin, { label = "", labelledby = "", describedby = "" }) =>
    h`<button class="switch" type="button" role="switch" data-toggle="${plugin.name}" aria-checked="${bool(plugin.on)}"
        ${label ? h`aria-label="${label}"` : h`aria-labelledby="${labelledby}" aria-describedby="${describedby}"`}
        ${attr(plugin.name === "ctl", 'disabled title="控制插件不能停用"')}></button>`;

  /** 宽屏详情栏的空位。不放一张大空白：把当前分类里最该先看的几个插件摆出来，
   *  鼠标不用在长列表里找，点一下就是详情。 */
  const detailPlaceholder = () => {
    const section = plugins.section ? sectionMeta(plugins.section) : null;
    const list = (plugins.data ? plugins.visible() : []).slice(0, 4);
    return h`<div class="card placeholder-card">
      <div class="placeholder-head">
        <span class="tile tile-large" data-tone="${plugins.section || "system"}" aria-hidden="true">${icon(section?.icon || "extension")}</span>
        <div class="placeholder-copy">
          <h2 class="placeholder-title">${section ? `从「${section.name}」开始` : "选一个插件"}</h2>
          <p class="placeholder-sub">${section ? section.hint : "左侧按用途分了类；点一行看它的开关、配置与指令。"}</p>
        </div>
      </div>
      ${list.length
        ? h`<ul class="group">${list.map(
            (plugin) => h`<li class="item" data-plugin="${plugin.name}">
              ${pluginTile(plugin)}
              <div class="item-content">
                <a class="item-link item-title" href="#/plugins/${encodeURIComponent(plugin.name)}">${plugin.display}<span class="tag">${plugin.name}</span></a>
                <span class="item-sub">${plugin.summary}</span>
              </div>
              ${switchButton(plugin, { label: `启用 ${plugin.display}` })}
            </li>`
          )}</ul>`
        : emptyState("这个分类里没有符合条件的插件。换个分类或清除筛选。", "search")}
    </div>`;
  };

  async function pluginsPage(route) {
    const [list, detail] = await Promise.all([
      api("/plugins"),
      route.arg ? api(`/plugins/${encodeURIComponent(route.arg)}`).catch((error) => error) : null,
    ]);
    plugins.data = list;
    plugins.selected = route.arg;
    const available = new Set(list.plugins.map((plugin) => plugin.section));
    if (plugins.section && !available.has(plugins.section)) plugins.section = "";
    if (!route.arg) {
      let saved = null;
      try {
        saved = localStorage.getItem("acumen.plugins.section");
      } catch {}
      if (saved) {
        if (available.has(saved)) plugins.section = saved;
        else {
          try {
            localStorage.removeItem("acumen.plugins.section");
          } catch {}
        }
      }
    }
    const dual = wide.matches;
    if (detail instanceof Error && detail.status === 401) throw detail;

    // 窄屏：详情单独成页，左上角是返回（HIG 的层级导航）。
    if (route.arg && !dual) {
      if (detail instanceof Error) throw detail;
      const meta = sectionMeta(detail.section);
      return h`<div class="page">
        ${pageHead(detail.display, detail.summary, { back: { href: "#/plugins", label: "插件" }, lead: pluginTile(detail, " tile-large"),
          meta: h`<span class="section-chip" data-tone="${detail.section}">${icon(meta.icon)}${meta.name}</span><span class="tag">${detail.name}</span>${statusTag(detail)}` })}
        ${detailBody(detail, 1)}
      </div>`;
    }

    const pane = !dual
      ? ""
      : h`<section class="split-detail" id="plugin-detail" aria-label="插件详情">${
          !detail
            ? detailPlaceholder()
            : detail instanceof Error
              ? h`<div class="card">${emptyState(detail.message, "error")}</div>`
              : detailBody(detail, 2)
        }</section>`;

    const title = detail && !(detail instanceof Error) ? detail.display : "";
    return h`<div class="page page-plugins"${title ? h` data-title="${title}"` : ""}>
      ${pageHead("插件", h`<span id="plugin-head-summary">${plugins.headline()}</span>`)}
      <div class="split"${attr(dual, "data-dual")}>
        <div class="split-list">
          <div class="plugin-toolbar">
            <div class="search">${icon("search")}
              <input class="input" id="plugin-search" type="search" placeholder="搜索插件、分类或指令"
                aria-label="搜索插件、分类或指令" autocomplete="off" value="${plugins.query}">
              <kbd class="search-key" aria-hidden="true">/</kbd>
            </div>
            <div class="filter-grid" id="plugin-filters" role="group" aria-label="筛选插件">${plugins.filters()}</div>
          </div>
          <p class="section-meta plugin-count-line"${attr(!plugins.filtered(), "hidden")}>
            <span id="plugin-count" role="status">${plugins.summary()}</span>
            <button class="btn btn-text btn-small" type="button" id="plugin-clear" data-clear-filters${attr(!plugins.filtered(), "hidden")}>清除筛选</button>
          </p>
          <div class="plugin-list" id="plugin-list">${plugins.rows()}</div>
        </div>
        ${pane}
      </div>
    </div>`;
  }

  /** 插件详情。`level` 是这一块最高的标题级别：宽屏在「插件」h1 之下用 h2，窄屏自成一页。 */
  function detailBody(plugin, level) {
    const top = `h${level}`;
    const sub = `h${Math.min(level + 1, 6)}`;
    const heading = (tag, id, text, cls = "section-title") => raw(`<${tag} class="${cls}" id="${id}">${esc(text)}</${tag}>`);
    const form = configForm(plugin);
    // 指令放在配置之前：配置一多，末尾的指令要滚很久才看得到。
    const commands = plugin.commands.length
      ? h`<section class="section" aria-labelledby="commands-title">
          <div class="section-head">
            ${heading(sub, "commands-title", "指令")}
            <span class="section-meta">${plugin.commands.length} 条 · 点按复制</span>
          </div>
          <ul class="group plate-list">${plugin.commands.map(
            (command) => h`<li><button class="item" type="button" data-copy="${command.cmd}" aria-label="复制指令 ${command.cmd}">
              <span class="item-content"><span class="item-title"><code class="tag">${command.cmd}</code></span>
              <span class="item-sub">${command.note}</span></span>
              <span class="item-trailing">${icon("content_copy")}</span></button></li>`
          )}</ul>
        </section>`
      : "";

    return h`
      ${level === 2
        ? h`<div class="detail-head">
            <div class="detail-lead">
              ${pluginTile(plugin, " tile-large")}
              ${raw(`<${top} class="detail-title" id="detail-title">`)}${plugin.display}<span class="tag">${plugin.name}</span>${statusTag(plugin)}${raw(`</${top}>`)}
            </div>
            <p class="note">${plugin.summary}</p>
            <p class="detail-section">${icon(sectionMeta(plugin.section).icon)}<span>${sectionMeta(plugin.section).name}</span><span class="detail-section-hint">${sectionMeta(plugin.section).hint}</span></p>
          </div>`
        : ""}
      <div class="card">
        <div class="toggle-row">
          <div class="field">
            <span class="field-label" id="enable-label">启用</span>
            <span class="field-hint" id="enable-hint">${plugin.effect}</span>
          </div>
          ${switchButton(plugin, { labelledby: "enable-label", describedby: "enable-hint" })}
        </div>
      </div>
      ${plugin.panel === "autoreply" ? rulesSection(sub) : ""}
      ${plugin.panel === "lurker" ? lurkerSection(sub) : ""}
      ${liveCard(plugin)}
      ${commands}
      ${plugin.name === "ambient" ? ambientSection(sub) : ""}

      <section class="section" aria-labelledby="config-title">
        <div class="section-head">
          ${heading(sub, "config-title", "配置")}
          <span class="section-meta">改完即保存：开关点一下，输入框离开或回车</span>
        </div>
        ${form.leaves >= 12
          ? h`<div class="config-tools">
              <div class="search">${icon("search")}
                <input class="input" id="config-filter-${plugin.name}" type="search" data-filter="${plugin.name}" placeholder="在 ${form.leaves} 项配置里找"
                  aria-label="筛选配置项" aria-describedby="config-filter-${plugin.name}-status" autocomplete="off" spellcheck="false" enterkeyhint="search">
              </div>
              <span class="section-meta" id="config-filter-${plugin.name}-status" role="status"></span>
            </div>`
          : ""}
        ${form.count
          ? h`<form class="config" data-config="${plugin.name}" autocomplete="off" novalidate>${form.html}</form>`
          : h`<p class="note">没有可以调整的配置项。</p>`}
        ${form.count
          ? h`<div class="actions"><button class="btn btn-outlined btn-danger" type="button" data-reset="${plugin.name}" data-display="${plugin.display}">恢复默认参数</button></div>`
          : ""}
      </section>

      <section class="section" aria-labelledby="diff-title">
        <div class="section-head">${heading(sub, "diff-title", "与默认值的差异")}</div>
        <div id="plugin-diff">${diffBlock(plugin.diff)}</div>
      </section>`;
  }

  const diffBlock = (diff) =>
    diff.length
      ? h`<pre class="code" tabindex="0" aria-label="与默认值的差异">${diff.join("\n")}</pre>`
      : h`<p class="note">全部与默认值一致。</p>`;

  let fieldSeq = 0;

  /** 群号列表：名单、开启搭话 / 推送的群等。其余的纯文字 / 数字 / 时间列表走列表编辑器（§11b）。 */
  const GROUP_LIST = /(^|\.)(channel\.(white|black)|groups|management_groups|management_excluded|realtime_muted_groups|manual_channels)$/;
  const USER_LIST = /(^|\.)(private_users|realtime_muted_private_users|admins|ignore_users|exempt)$/;
  /** 这一项是不是 ID 名单，是哪一种；不是（或混着数字对象）就返回空串。 */
  const listKind = (path, value) =>
    !value.every((item) => typeof item === "string") ? "" : GROUP_LIST.test(path) ? "group" : USER_LIST.test(path) ? "user" : "";

  const isTable = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
  const HIDDEN = "<已隐藏>";
  /** 与后端 ctl::sensitive 同一张词表：这些键的值不回显，输入框只收新值。 */
  const SECRET = /token|secret|password|api_?key|authorization|credential|cookie/i;
  const TIME = /^\d{1,2}:\d{2}(:\d{2})?$/;
  /** 0—1 的比例（概率类配置）：表单里换成百分数滑块，手机上不必敲「0.35」。 */
  const RATIO = /(^|\.|_)(interrupt_)?probability$/;
  const valueAt = (root, path) =>
    path.split(".").reduce((node, key) => (node !== null && typeof node === "object" ? node[key] : undefined), root);
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

  /** 每个插件表单的上下文：说明、可选项、映射表与默认值。行内「恢复默认」「已改」都要它。 */
  const configs = new Map();
  /** 映射表里展开着的项（插件:路径）。重画这一块时照原样展开，不让人重新找。 */
  const openEntries = new Set();

  /** 字段说明：配置参考里的那句话；映射表里一项的字段看映射表自带的说明。 */
  function helpFor(ctx, path) {
    const direct = ctx.help[path] || ctx.help[path.replace(/\.\d+(?=\.|$)/g, "")];
    if (direct) return direct;
    const inside = mapOf(ctx, path);
    return inside?.field ? inside.spec.help?.[inside.field] || "" : "";
  }

  /** 路径落在哪张映射表里：表的路径（具体的）、登记的写法（可以带 `*`）、说明、键，以及（如果是项里的字段）字段名。
   *  一项里的字段自己也可以是一张映射表（每群的自定义占位符），所以挑最深的那一张。 */
  function mapOf(ctx, path) {
    const parts = path.split(".");
    let found = null;
    for (const [pattern, spec] of Object.entries(ctx.maps)) {
      const head = pattern.split(".");
      if (parts.length <= head.length || !head.every((want, index) => want === "*" || want === parts[index])) continue;
      const [key, field = "", ...rest] = parts.slice(head.length);
      if (rest.length || (found && found.depth >= head.length)) continue;
      found = { map: parts.slice(0, head.length).join("."), pattern, spec, key, field, depth: head.length };
    }
    return found;
  }

  /** 这一项本身是不是一张映射表；登记的写法里的 `*` 匹配任意一个键。 */
  function mapSpec(ctx, path) {
    if (ctx.maps[path]) return ctx.maps[path];
    const parts = path.split(".");
    const hit = Object.entries(ctx.maps).find(([pattern]) => {
      const head = pattern.split(".");
      return head.length === parts.length && head.every((want, index) => want === "*" || want === parts[index]);
    });
    return hit ? hit[1] : null;
  }

  /** 默认值的一句话说法，给「恢复默认」的名字用。 */
  function describeDefault(value) {
    if (typeof value === "boolean") return value ? "开" : "关";
    if (Array.isArray(value)) return value.length ? `${value.length} 项` : "空列表";
    if (typeof value === "string") return value ? `「${[...value].length > 24 ? `${[...value].slice(0, 24).join("")}…` : value}」` : "留空";
    return String(value);
  }

  /** 这一行和默认值比怎么样、尾部给什么动作：
   *  普通项改过了就有「已改」和「恢复默认」；映射表里一项的字段可以「沿用上层」（删掉这一项的单独设置）。
   *  开关只给「已改」——再点一下就是恢复，不必多一个按钮。 */
  function rowMeta(ctx, path, value, key) {
    const inside = mapOf(ctx, path);
    if (inside?.field) {
      return {
        modified: false,
        action: inside.spec.optional
          ? h`<button class="icon-btn state field-reset" type="button" data-inherit="${path}"
              aria-label="不再单独设置 ${key}，沿用上层" title="不再单独设置，沿用上层">${icon("settings_backup_restore")}</button>`
          : "",
      };
    }
    const fallback = valueAt(ctx.defaults, path);
    if (fallback === undefined || SECRET.test(key) || same(fallback, HIDDEN)) return { modified: false, action: "" };
    const modified = !same(value, fallback);
    return {
      modified,
      action: typeof value === "boolean"
        ? ""
        : h`<button class="icon-btn state field-reset" type="button" data-reset-path="${path}"${attr(!modified, "hidden")}
            aria-label="${key} 恢复默认：${describeDefault(fallback)}" title="恢复默认：${describeDefault(fallback)}">${icon("settings_backup_restore")}</button>`,
    };
  }

  /** 改完之后重判这一行是不是还与默认值不同。 */
  function markModified(row, value) {
    const form = row?.closest("[data-config]");
    const ctx = form && configs.get(form.dataset.config);
    if (!ctx || mapOf(ctx, row.dataset.setting)?.field) return;
    const fallback = valueAt(ctx.defaults, row.dataset.setting);
    if (fallback === undefined) return;
    const modified = !same(value, fallback);
    row.toggleAttribute("data-modified", modified);
    const tag = $(".field-top .mod-tag", row);
    if (tag) tag.hidden = !modified;
    const reset = $(".field-top [data-reset-path]", row);
    if (reset) {
      // 焦点正落在要藏起来的按钮上：先交给这一行的控件，免得焦点掉回页首。
      if (!modified && reset === document.activeElement) $("[data-path]:not([hidden]), .tags-add input, .switch", row)?.focus({ preventScroll: true });
      reset.hidden = !modified;
    }
  }

  /** 插件详情里画表单要的那几样，进表单上下文（重取详情时也走这里，免得两处各抄一份）。 */
  function absorb(ctx, plugin) {
    Object.assign(ctx, {
      help: plugin.field_help || {},
      options: plugin.field_options || {},
      maps: plugin.maps || {},
      defaults: plugin.defaults || {},
      config: plugin.config || {},
      order: plugin.field_order || [],
      labels: plugin.labels || {},
      optionLabels: plugin.option_labels || {},
      depends: plugin.depends || {},
      templates: plugin.templates || null,
    });
  }

  /** 映射表里一项的字段写成 `表.*.字段`：中文名、先后、模板说明都按这个通配路径登记。 */
  function starPath(ctx, path) {
    const inside = mapOf(ctx, path);
    if (!inside) return path;
    return inside.field ? `${inside.pattern}.*.${inside.field}` : `${inside.pattern}.*`;
  }

  const labelOf = (ctx, path) => ctx.labels?.[path] || ctx.labels?.[starPath(ctx, path)] || "";
  const optionLabel = (ctx, path, value) => (ctx.optionLabels?.[path] || ctx.optionLabels?.[starPath(ctx, path)] || {})[value] || "";

  /** 同一层里的先后：照配置参考里写的顺序（后端 field_order），没登记的按键名排在后面。 */
  function byOrder(ctx, base) {
    const rank = (key) => {
      const index = ctx.order.indexOf(starPath(ctx, base ? `${base}.${key}` : key));
      return index < 0 ? Infinity : index;
    };
    return ([a], [b]) => rank(a) - rank(b) || (a < b ? -1 : a > b ? 1 : 0);
  }

  /** 名字：有中文名时中文名在前、键名作小字跟在后面（`/ctl` 认的还是键名）；没有就只有键名。 */
  const keyName = (ctx, path, key) => {
    const label = labelOf(ctx, path);
    return label ? h`<span class="field-name">${label}</span> <code class="field-key">${key}</code>` : h`<code>${key}</code>`;
  };

  /** 有了中文名，说明开头再念一遍名字（「超时怎么办：……」「入群验证：……」）就是重复，剪掉。 */
  function plainHelp(ctx, path, text) {
    const parent = path.split(".").slice(0, -1).join(".");
    for (const name of [labelOf(ctx, path), parent && labelOf(ctx, parent)]) {
      if (name && text.startsWith(`${name}：`)) return text.slice(name.length + 1);
    }
    return text;
  }

  /** 以秒计的数字旁边换算成人话（300 → 5 分钟），改的时候跟着变。 */
  const SECONDS = /_seconds$/;
  function secondsText(value) {
    const n = Number(value);
    if (!Number.isFinite(n) || n <= 0 || !Number.isInteger(n)) return "";
    if (n % 86400 === 0) return `${n / 86400} 天`;
    if (n % 3600 === 0) return `${n / 3600} 小时`;
    if (n >= 60) return n % 60 ? `${Math.floor(n / 60)} 分 ${n % 60} 秒` : `${n / 60} 分钟`;
    return "";
  }

  /** 以毫秒计的数字旁边换算成人话（1200 → 1.2 秒；0 → 不停顿）。
   *  延迟这一类数量在手机上敲的是毫秒，读的时候想的是「一秒上下」。 */
  const MILLIS = /_ms$/;
  function millisText(value) {
    const n = Number(value);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) return "";
    if (n === 0) return "不停顿";
    if (n < 1000) return `${n} 毫秒`;
    return `${Number((n / 1000).toFixed(2))} 秒`;
  }

  /** 只在某个取值下才用得到的项：那一项现在不是这些值时，行里说一句。 */
  function mootNote(ctx, path) {
    const rule = ctx.depends?.[path];
    if (!rule) return "";
    const current = valueAt(ctx.config, rule.on);
    const wanted = rule.in.map((value) => `「${optionLabel(ctx, rule.on, value) || value}」`).join("或");
    const name = labelOf(ctx, rule.on) || rule.on;
    return h`<p class="field-moot" data-moot-on="${rule.on}" data-moot-in="${rule.in.join(" ")}"${attr(rule.in.includes(current), "hidden")}>
      ${icon("info")}<span>${name}选${wanted}时才用得到。</span></p>`;
  }

  /** 下拉框改了：依赖它的那几行跟着显示或收起「用不到」的说明。 */
  function paintMoot(form, path, value) {
    for (const note of $$(`[data-moot-on="${CSS.escape(path)}"]`, form)) note.hidden = note.dataset.mootIn.split(" ").includes(value);
  }

  /** 配置表单。叶子收成一组分段列表（顶层叫「常规」），每个子表另起一块：
   *  同一层里叶子在前、子表在后，分段列表才不会被一张子表从中间隔开。 */
  function configForm(plugin) {
    const ctx = { name: plugin.name };
    absorb(ctx, plugin);
    configs.set(plugin.name, ctx);
    const entries = Object.entries(plugin.config).filter(([key]) => key !== "enabled").sort(byOrder(ctx, ""));
    const field = ([key, value]) => configField(key, key, value, ctx);
    const loose = entries.filter(([, value]) => !isTable(value));
    const tables = entries.filter(([, value]) => isTable(value));
    const leaves = (value) => (isTable(value) ? Object.values(value).reduce((sum, child) => sum + leaves(child), 0) : 1);
    return {
      count: entries.length,
      leaves: entries.reduce((sum, [, value]) => sum + leaves(value), 0),
      html: [
        loose.length
          ? h`<fieldset class="config-group" data-search="常规"><legend class="config-legend${tables.length ? "" : " visually-hidden"}">常规</legend>${loose.map(field)}</fieldset>`
          : "",
        ...tables.map(field),
      ],
    };
  }

  const legendName = (ctx, path, key) => {
    const label = labelOf(ctx, path);
    return label ? h`<span class="legend-name">${label}</span><code class="legend-key">${key}</code>` : key;
  };

  /** 子表里的排序：总开关（enabled）打头，叶子在前，子表在后。 */
  const fieldOrder = ([keyA, a], [keyB, b]) =>
    (keyB === "enabled" && typeof b === "boolean") - (keyA === "enabled" && typeof a === "boolean") || isTable(a) - isTable(b);

  /** 说明文字。超过两行的先收起来，旁边一枚「展开」；读屏照样读全文（只是视觉上截断）。 */
  const LONG_HINT = 64;
  const hint = (id, text) =>
    String(text).length > LONG_HINT
      ? h`<span class="field-hint" id="${id}" data-clamp>${text}</span><button class="hint-more" type="button" data-hint-more="${id}" aria-expanded="false" aria-controls="${id}">展开</button>`
      : h`<span class="field-hint" id="${id}">${text}</span>`;

  /** 一个配置项。子表展开成 fieldset，叶子就地成为能改的控件：
   *  名字（键）与说明各占一行，说明挂在控件的 aria-describedby 上，不混进控件的名字。 */
  function configField(path, key, value, ctx, { master = false } = {}) {
    const id = `f${++fieldSeq}`;
    const description = templateOf(ctx, path)?.about || plainHelp(ctx, path, helpFor(ctx, path));
    const desc = description ? `${id}-desc` : "";
    const meta = rowMeta(ctx, path, value, key);
    const search = `${path} ${labelOf(ctx, path)} ${description}`.toLowerCase();
    const text = (label) => h`<div class="field-text">
        <div class="field-top">${label}${master ? h`<span class="tag">总开关</span>` : ""}<span class="mod-tag"${attr(!meta.modified, "hidden")}>已改</span>${meta.action}</div>
        ${description ? hint(desc, description) : ""}
        ${mootNote(ctx, path)}
      </div>`;
    const name = keyName(ctx, path, key);
    const named = h`<label class="field-label" for="${id}">${name}</label>`;
    const row = (cls, inner, extra = "") =>
      h`<div class="${cls} setting" data-setting="${path}" data-search="${search}"${attr(meta.modified, "data-modified")}${raw(extra)}>${inner}</div>`;

    if (isTable(value) && mapSpec(ctx, path)) return mapEditor(path, key, value, mapSpec(ctx, path), ctx);
    if (isTable(value)) {
      const children = Object.entries(value).sort((a, b) => fieldOrder(a, b) || byOrder(ctx, path)(a, b));
      const hasMaster = typeof value.enabled === "boolean";
      const about = ctx.help[path];
      return h`<fieldset class="config-group" data-group="${path}" data-search="${`${path} ${labelOf(ctx, path)} ${about || ""}`.toLowerCase()}"${attr(hasMaster && !value.enabled, "data-off")}>
        <legend class="config-legend">${legendName(ctx, path, key)}</legend>
        ${about ? h`<p class="config-about">${about}</p>` : ""}
        ${children.map(([child, item], index) =>
          index === 0 && hasMaster
            ? h`${configField(`${path}.${child}`, child, item, ctx, { master: true })}
                <p class="config-off-note">${icon("info-fill")}<span>总开关关着，这一节的其余设置暂不生效；可以先改好再打开。</span></p>`
            : configField(`${path}.${child}`, child, item, ctx)
        )}
      </fieldset>`;
    }
    if (ctx.options[path]?.length) {
      return row("field", h`${text(named)}
        <span class="select"><select id="${id}" data-path="${path}" data-kind="string" data-initial="${value}"${desc ? h` aria-describedby="${desc}"` : ""}>
        ${[...new Set([value, ...ctx.options[path]])].map((option) => h`<option value="${option}"${attr(option === value, "selected")}>${optionLabel(ctx, path, option) || option}</option>`)}
        </select>${icon("keyboard_arrow_down")}</span>`, ' data-row="short"');
    }
    if (Array.isArray(value)) {
      const kind = listKind(path, value);
      if (kind) {
        const role = /(^|\.)channel\.white$/.test(path) ? "channel-white" : /(^|\.)channel\.black$/.test(path) ? "channel-black" : "";
        const fallback = { "channel-white": "非空时，只在名单里的群生效。", "channel-black": "名单里的群一律不生效，优先于白名单。" }[role];
        const hint = description || fallback || (kind === "user" ? "用户账号列表。" : "群号列表。");
        const title = { "channel-white": "白名单", "channel-black": "黑名单" }[role] || labelOf(ctx, path);
        return row("", picker({
          id, kind, mode: "save", ids: value, path, role, title: title || key,
          label: h`${title ? h`<span>${title}</span> ` : ""}<code>${key}</code><span class="field-hint">${hint}</span>`,
        }));
      }
      const spec = templateOf(ctx, path);
      if (spec) return row("tpl-row", h`${text(h`<span class="field-label" id="${id}-label">${name}</span>`)}${templateBody(id, path, value, ctx, spec)}`);
      const shape = listShape(key, value, valueAt(ctx.defaults, path));
      if (shape) {
        return row("", listEditor({
          id, type: shape, items: value, mode: "save", path, title: labelOf(ctx, path) || key, describedby: desc, plugin: ctx.name,
          head: text(h`<span class="field-label" id="${id}-label">${name}</span>`),
        }));
      }
      // 对象列表、混着类型或带隐藏凭据的列表：只有 JSON 能把它原样说清。
      return row("field", h`${text(named)}
        <textarea class="textarea mono textarea-json" id="${id}" data-path="${path}" data-kind="list" spellcheck="false" autocapitalize="off"
          aria-describedby="${[desc, `${id}-hint`].filter(Boolean).join(" ")}">${JSON.stringify(value, null, 2)}</textarea>
        <span class="field-hint" id="${id}-hint">JSON 列表：保留引号以区分文本和数字，空列表写 []。离开输入框保存。</span>`);
    }
    if (typeof value === "boolean") {
      return row("toggle-row", h`${text(h`<span class="field-label" id="${id}">${name}</span>`)}
        <button class="switch" type="button" role="switch" data-path="${path}" data-kind="bool"${attr(master, "data-master")}
          aria-checked="${bool(value)}" aria-labelledby="${id}"${desc ? h` aria-describedby="${desc}"` : ""}></button>`, master ? ' data-master=""' : "");
    }
    if (typeof value === "string" && SECRET.test(key)) {
      // 凭据不回显：框里永远是空的，占位说明有没有设过；只有亲手输入的新值才会提交。
      const set = value !== "";
      return row("field", h`${text(named)}
        <div class="reveal">
          <input class="input mono" id="${id}" type="password" data-path="${path}" data-kind="string" data-secret="${set ? "set" : ""}" value=""
            placeholder="${set ? "已设置（不显示）；输入新值即替换" : "未设置"}" autocomplete="new-password" spellcheck="false" autocapitalize="off"
            data-1p-ignore data-lpignore="true" data-bwignore="true"${desc ? h` aria-describedby="${desc}"` : ""}>
          <button class="icon-btn state" type="button" data-reveal="#${id}" aria-pressed="false" aria-label="显示口令">${icon("visibility")}</button>
        </div>`);
    }
    if (typeof value === "string" && templateOf(ctx, path)) {
      return row("tpl-row", h`${text(h`<span class="field-label" id="${id}-label">${name}</span>`)}${templateBody(id, path, value, ctx, templateOf(ctx, path))}`);
    }
    if (typeof value === "string" && (value.includes("\n") || value.length > 80)) {
      return row("field", h`${text(named)}
        <textarea class="textarea" id="${id}" data-path="${path}" data-kind="string" spellcheck="false"${desc ? h` aria-describedby="${desc}"` : ""}>${value}</textarea>`);
    }
    // 概率：滑块 + 一个能敲的百分数框，两个入口改的是同一个值。
    // 拖动时右边跟着变、松手落盘；敲进框里回车或离开也落盘，超出 0—100 收回来。
    if (typeof value === "number" && RATIO.test(path) && value >= 0 && value <= 1) {
      const pct = Math.round(value * 100);
      const label = labelOf(ctx, path) || key;
      return row("field ratio-field", h`${text(h`<label class="field-label" for="${id}" id="${id}-label">${name}</label>`)}
        <div class="ratio">
          <input class="slider" type="range" id="${id}" data-path="${path}" data-kind="ratio" min="0" max="100" step="1"
            value="${pct}" style="--_fill:${pct}%" aria-valuetext="${pct}%" spellcheck="false"
            ${desc ? h` aria-describedby="${desc}"` : ""}>
          <label class="ratio-box">
            <input class="ratio-input num" type="number" min="0" max="100" step="1" inputmode="numeric" spellcheck="false"
              value="${pct}" aria-label="${label}（百分数，0 到 100）"${desc ? h` aria-describedby="${desc}"` : ""}>
            <span class="ratio-unit" aria-hidden="true">%</span>
          </label>
        </div>`);
    }
    const numeric = typeof value === "number";
    const clock = typeof value === "string" && TIME.test(value);
    const unit = numeric && SECONDS.test(key) ? "s" : numeric && MILLIS.test(key) ? "ms" : "";
    const input = h`<input class="input${numeric || clock ? " num" : " mono"}" id="${id}" data-path="${path}" data-kind="${numeric ? "number" : "string"}"
        value="${value ?? ""}" spellcheck="false" autocapitalize="off"${[desc, unit ? `${id}-unit` : ""].some(Boolean) ? h` aria-describedby="${[desc, unit ? `${id}-unit` : ""].filter(Boolean).join(" ")}"` : ""}${attr(numeric, `inputmode="${Number.isInteger(value) ? "numeric" : "decimal"}"`)}>`;
    return row("field", h`${text(named)}
      ${unit
        ? h`<span class="input-unit">${input}<span class="unit-text num" id="${id}-unit" data-unit-for="${id}" data-unit="${unit}">${unit === "ms" ? millisText(value) : secondsText(value)}</span></span>`
        : input}`,
      numeric || clock ? ' data-row="short"' : "");
  }

  function parseValue(kind, text) {
    // 滑块给的是 0—100 的整数，配置里收 0—1：留三位小数，够 1% 的粒度又不带浮点尾巴。
    if (kind === "ratio") {
      const value = Number(text) / 100;
      if (!Number.isFinite(value)) throw new Error("这一项需要一个 0 到 100 的百分数。");
      return Math.round(value * 1000) / 1000;
    }
    if (kind === "number") {
      const value = Number(text.trim());
      if (!text.trim() || !Number.isFinite(value)) throw new Error("这一项需要一个数字。");
      return value;
    }
    if (kind !== "list") return text;
    try {
      const value = JSON.parse(text.trim() || "[]");
      if (!Array.isArray(value)) throw new Error();
      if (JSON.stringify(value).includes(HIDDEN)) throw new Error("masked");
      return value;
    } catch (error) {
      if (error.message === "masked") throw new Error("列表含有已隐藏的凭据。请提供完整的新值，或使用维护命令按具体字段修改。");
      throw new Error('请输入 JSON 列表，例如 ["文字", "含,逗号"] 或 [123, 456]。');
    }
  }

  /** 就地校验：错在哪一格就在哪一格下面说，并立即播报。 */
  function fieldError(control, message) {
    clearFieldError(control);
    control.setAttribute("aria-invalid", "true");
    const note = document.createElement("p");
    note.className = "field-error";
    note.id = `${control.id}-error`;
    note.setAttribute("role", "alert");
    note.innerHTML = part(h`${icon("error-fill")}<span>${message}</span>`);
    (control.closest(".reveal, .tags-add") ?? control).after(note);
    const described = (control.getAttribute("aria-describedby") || "").split(" ").filter(Boolean);
    control.setAttribute("aria-describedby", [...described, note.id].join(" "));
  }

  function clearFieldError(control) {
    const note = document.getElementById(`${control.id}-error`);
    if (!note) return;
    note.remove();
    control.removeAttribute("aria-invalid");
    const rest = (control.getAttribute("aria-describedby") || "").split(" ").filter((id) => id && id !== note.id);
    if (rest.length) control.setAttribute("aria-describedby", rest.join(" "));
    else control.removeAttribute("aria-describedby");
  }

  const configPost = async (form, endpoint, body) => {
    const reply = await api(`/plugins/${encodeURIComponent(form.dataset.config)}/${endpoint}`, { method: "POST", body });
    refreshLive(form.dataset.config);
    return reply;
  };

  /** 插件「现在的情况」（去处、这次运行的计数）。只有后端给了 live 的插件才有这张卡。 */
  const liveLines = (live) => h`<ul class="live-lines">${live.lines.map((line) => h`<li>${line}</li>`)}</ul>`;
  const liveCard = (plugin) =>
    plugin.live
      ? h`<section class="card live" id="live-${plugin.name}" aria-labelledby="live-${plugin.name}-title">
          <h3 class="live-title" id="live-${plugin.name}-title">现在的情况</h3>
          <div class="live-body" aria-live="polite">${liveLines(plugin.live)}</div>
          ${plugin.live.test
            ? h`<div class="actions"><button class="btn btn-tonal" type="button" data-live-test="${plugin.name}">${icon("send")}发一条试试</button></div>`
            : ""}
        </section>`
      : "";

  async function refreshLive(name) {
    const card = $(`#live-${CSS.escape(name)}`);
    if (!card) return;
    const plugin = await api(`/plugins/${encodeURIComponent(name)}`).catch(() => null);
    if (plugin?.live) put($(".live-body", card), liveLines(plugin.live));
  }

  async function testLive(button) {
    if (button.getAttribute("aria-busy") === "true") return;
    const name = button.dataset.liveTest;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = await api(`/plugins/${encodeURIComponent(name)}/test`, { method: "POST", body: {} });
      snackbar(reply.message);
      if (reply.live) put($(`#live-${CSS.escape(name)} .live-body`), liveLines(reply.live));
    } catch (error) {
      report(error);
    } finally {
      button.removeAttribute("aria-busy");
    }
  }

  /** 概率的这一格：把百分数同时写到滑块、填充、读屏值与右边的数字框上。 */
  function paintRatio(slider, pct) {
    const pctText = String(pct);
    slider.value = pctText;
    slider.style.setProperty("--_fill", `${pctText}%`);
    slider.setAttribute("aria-valuetext", `${pctText}%`);
    const box = slider.closest(".ratio")?.querySelector(".ratio-input");
    if (box && box.value !== pctText) box.value = pctText;
  }

  const syncRatio = (slider) => paintRatio(slider, Math.round(Number(slider.value)));

  /** 在数字框里敲完：收进 0—100，两个控件对齐，再走一次正常的保存。 */
  async function commitRatio(box) {
    const slider = box.closest(".ratio")?.querySelector(".slider");
    if (!slider) return;
    // 清空了就当没改：空框不等于 0（0 是「永不跟读」，不能靠删一刀误触）。
    const raw = box.value.trim();
    const typed = raw === "" ? Number.NaN : Math.round(Number(raw));
    const pct = Number.isFinite(typed) ? Math.min(100, Math.max(0, typed)) : Math.round(Number(slider.value));
    paintRatio(slider, pct);
    await commitField(slider);
  }

  async function commitField(control) {
    const form = control.closest("[data-config]");
    if (!form || control.disabled || control.getAttribute("aria-busy") === "true") return;
    const kind = control.dataset.kind;
    const submitted = control.value;
    let saved = false;
    let value;
    if (kind === "bool") value = control.getAttribute("aria-checked") !== "true";
    else {
      if (control.value === (control.defaultValue ?? control.dataset.initial)) return;
      // 凭据框只认亲手输入的新值：留空是「不改」，自动填进来的不算。
      if (control.dataset.secret !== undefined && (!control.value || !typed.has(control))) return;
      try {
        value = parseValue(kind, control.value);
      } catch (error) {
        fieldError(control, error.message);
        return;
      }
    }
    control.setAttribute("aria-busy", "true");
    try {
      const reply = await configPost(form, "config", { path: control.dataset.path, value });
      if (kind === "bool") {
        control.setAttribute("aria-checked", String(value));
        if (control.dataset.master !== undefined) control.closest("fieldset")?.toggleAttribute("data-off", !value);
      } else if (control.dataset.secret !== undefined) {
        control.value = "";
        control.dataset.secret = "set";
        control.placeholder = "已设置（不显示）；输入新值即替换";
      } else control.defaultValue = submitted;
      saved = true;
      clearFieldError(control);
      markModified(control.closest("[data-setting]"), value);
      if (control.tagName === "SELECT") paintMoot(form, control.dataset.path, value);
      snackbar(reply.message);
      refreshDiff(form.dataset.config);
    } catch (error) {
      if (error.status === 401) return;
      if (kind === "bool") report(error);
      else fieldError(control, error.message);
    } finally {
      control.removeAttribute("aria-busy");
      if (saved && control.isConnected && kind !== "bool" && control.dataset.secret === undefined && control.value !== submitted) void commitField(control);
    }
  }

  /** 取一次插件详情，更新表单上下文（默认值、映射表模板会跟着当前配置变）。 */
  async function freshPlugin(name) {
    const plugin = await api(`/plugins/${encodeURIComponent(name)}`);
    const ctx = configs.get(name);
    if (ctx) {
      absorb(ctx, plugin);
      repaintPreviews(name);
    }
    if ($(`[data-config="${CSS.escape(name)}"]`)) put($("#plugin-diff"), diffBlock(plugin.diff));
    return plugin;
  }

  async function refreshDiff(name) {
    try {
      await freshPlugin(name);
    } catch {
      /* 差异只是参考信息，读不到不打扰 */
    }
  }

  /** 把一行换成新画的一行（值来自刚取回的配置），焦点交给新行里同一类控件。 */
  function replaceRow(row, plugin, ctx) {
    const path = row.dataset.setting;
    const key = path.split(".").pop();
    const holder = document.createElement("div");
    put(holder, configField(path, key, valueAt(plugin.config, path), ctx));
    const fresh = holder.firstElementChild;
    row.replaceWith(fresh);
    return fresh;
  }

  /** 行内「恢复默认」：只重置这一项（ctl reset 插件 路径），别的不动。 */
  async function resetField(button) {
    const row = button.closest("[data-setting]");
    const form = button.closest("[data-config]");
    if (!row || !form || button.getAttribute("aria-busy") === "true") return;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = await configPost(form, "reset", { path: row.dataset.setting });
      const plugin = await freshPlugin(form.dataset.config);
      const fresh = replaceRow(row, plugin, configs.get(form.dataset.config));
      $("[data-path]:not([hidden]), [data-tpl-input], .tags-add input, .picker-add input", fresh)?.focus({ preventScroll: true });
      snackbar(reply.message.replace(/^.*?(已|恢复)/, "$1"));
    } catch (error) {
      button.removeAttribute("aria-busy");
      report(error);
    }
  }

  /* ---------- 文案编辑器：带占位符的提示（几种说法随机一种、点按插入占位符、就地预览） ---------- */

  /** 这一项是不是提示模板；是就给它的说明（哪一种提示、一句用途）。 */
  const templateOf = (ctx, path) =>
    ctx.templates?.fields?.[path] || ctx.templates?.fields?.[starPath(ctx, path)] || ctx.templates?.fallback || null;
  /** 这一项属于哪个群的单独设置（`groups.群号.…`）：那个群自己的自定义占位符只在它的提示里认得。 */
  const groupOf = (path) => /^groups\.([^.]+)\./.exec(path)?.[1] || "";
  const tplSpec = (ctx, path) => {
    const spec = templateOf(ctx, path);
    return spec ? { ...spec, group: groupOf(path) } : null;
  };
  /** 这种提示能用的占位符：内置的、全局的自定义占位符，再叠上本群自己的（同名的顶掉全局那份）。 */
  function holdersFor(ctx, kind, group = "") {
    const local = (ctx.templates?.group_custom?.[group]?.placeholders || []).filter((holder) => holder.kinds.includes(kind));
    const shared = (ctx.templates?.placeholders || []).filter((holder) => holder.kinds.includes(kind) && !local.some((own) => own.key === holder.key));
    return [...shared, ...local];
  }
  const variantsOf = (value) => (Array.isArray(value) ? value.map(String) : typeof value === "string" && value ? [value] : []);

  /** 「插入」那一排芯片。自定义占位符带一枚 {} 图标，和内置的分得开。表情这类只给认得它的提示。 */
  function holderChips(ctx, spec) {
    const extras = (ctx.templates?.extras || []).filter((extra) => !extra.kinds || extra.kinds.includes(spec.kind));
    return h`${holdersFor(ctx, spec.kind, spec.group).map((holder) => h`<button class="chip tpl-holder${holder.custom ? " tpl-own" : ""} state" type="button" data-tpl-insert="{${holder.key}}"
        title="{${holder.key}}${holder.hint ? ` · ${holder.hint}` : ""}" aria-label="插入${holder.custom ? "自定义占位符" : ""}${holder.label}{${holder.key}}${holder.hint ? `：${holder.hint}` : ""}">${holder.custom ? icon("data_object") : ""}${holder.label}</button>`)}
      ${extras.map((extra) => h`<button class="chip tpl-holder tpl-extra state" type="button" data-tpl-insert="${extra.insert}"
          title="${extra.hint}" aria-label="插入${extra.label}：${extra.hint}">${extra.label}</button>`)}`;
  }

  function templateBody(id, path, value, ctx, base) {
    const variants = variantsOf(value);
    const spec = { ...base, group: groupOf(path) };
    const silent = spec.silent || (spec.kind === "custom" ? "一种说法都没有：插进去是空的。" : "一种说法都没有：这条提示不发。");
    return h`<div class="tpl" id="${id}" data-tpl="${path}" data-tpl-kind="${spec.kind}" data-tpl-saved="${JSON.stringify(variants)}">
      <ol class="tpl-list" aria-label="${spec.about}">${variants.map((text, index) => templateVariant(id, index, text, ctx, spec))}</ol>
      <p class="tpl-silent"${attr(variants.length, "hidden")}>${icon("notifications_off")}<span>${silent}</span></p>
      <div class="tpl-tools">
        <span class="tpl-tools-label" id="${id}-insert">插入</span>
        <div class="tpl-holders" role="group" aria-labelledby="${id}-insert">${holderChips(ctx, spec)}</div>
        <div class="tpl-more">
          <button class="btn btn-text tpl-add" type="button" data-tpl-add>${icon("add")}${variants.length ? "再加一种说法" : "写一句"}</button>
          ${spec.images
            ? h`<button class="btn btn-text tpl-image" type="button" data-tpl-image>${icon("image")}<span>插入图片</span></button>
                <input class="tpl-image-file" type="file" accept="image/*" hidden aria-hidden="true" tabindex="-1">`
            : ""}
        </div>
      </div>
    </div>`;
  }

  /** 自定义占位符加了、删了、改了：各编辑器的插入芯片与预览跟着换。 */
  function repaintHolders(name) {
    const ctx = configs.get(name);
    if (!ctx) return;
    for (const root of $$(`[data-config="${CSS.escape(name)}"] [data-tpl]`)) {
      const spec = tplSpec(ctx, root.dataset.tpl);
      const row = $(".tpl-holders", root);
      if (spec && row && !row.contains(document.activeElement)) put(row, holderChips(ctx, spec));
    }
    repaintPreviews(name);
  }

  function templateVariant(id, index, text, ctx, spec) {
    const field = `${id}-v${index}`;
    const { html, problems } = previewOf(text, ctx, spec);
    return h`<li class="tpl-item" data-tpl-index="${index}">
      <div class="tpl-item-head">
        <label class="tpl-item-label" for="${field}">说法 ${index + 1}</label>
        <button class="icon-btn state tpl-remove" type="button" data-tpl-remove="${index}" aria-label="删掉说法 ${index + 1}" title="删掉这种说法">${icon("delete")}</button>
      </div>
      <textarea class="textarea tpl-input" id="${field}" data-tpl-input rows="2" spellcheck="false" autocapitalize="off"
        aria-describedby="${field}-problems ${field}-preview">${text}</textarea>
      <p class="field-hint tpl-problems" id="${field}-problems" role="status">${problems}</p>
      <div class="tpl-preview" id="${field}-preview" aria-label="预览">
        <span class="tpl-avatar" aria-hidden="true">${icon("smart_toy")}</span>
        <div class="tpl-bubble">${html}</div>
      </div>
    </li>`;
  }

  /** 模板在群里大概长什么样：占位符换成样例值，@ 与图、表情画成小块。
   *  认不出、或这条提示里用不了的占位符原样留着并标出来——发出去也是原样，不如现在就看见。
   *  占位符的说法（`spec.plain`）只是一句话：套进一条真用到它的提示里预览（`spec.host`），这句垫淡底。 */
  function previewOf(text, ctx, spec, depth = 0, inject = null) {
    if (spec.host && depth === 0 && text.trim()) {
      const own = previewOf(text, ctx, { ...spec, host: null }, 1);
      const host = previewOf(spec.host.text, ctx, { kind: spec.host.kind, group: "" }, 0, { [spec.host.slot]: own.html });
      return { html: host.html, problems: own.problems };
    }
    const own = ctx.templates?.group_custom?.[spec.group || ""];
    const samples = { ...(ctx.templates?.samples || {}), ...(own?.samples || {}), ...(spec.samples || {}) };
    const holders = holdersFor(ctx, spec.kind, spec.group);
    const allowed = new Set(holders.map((holder) => holder.key));
    const snippets = new Set(holders.filter((holder) => holder.custom).map((holder) => holder.key));
    const bad = [];
    const pill = (glyph, label) => `<span class="tpl-pill">${part(icon(glyph))}${esc(label)}</span>`;
    let html = "";
    let last = 0;
    let images = false;
    for (const match of text.matchAll(/\{([^{}]*)\}/g)) {
      html += esc(text.slice(last, match.index));
      last = match.index + match[0].length;
      const inner = match[1].trim();
      if (inject && inner in inject) html += `<span class="tpl-snippet tpl-phrase" title="{${esc(inner)}} 的这句说法">${part(inject[inner])}</span>`;
      else if (inner === "at" && allowed.has("at")) html += `<span class="tpl-at">@${esc(samples.name || "小林")}</span>`;
      else if (inner === "image" && allowed.has("image")) {
        images = true;
        html += pill("image", "配图");
      } else if (inner === "avatar" && !spec.plain) html += pill("account_circle", "头像");
      else if (/^face:\d+$/.test(inner) && !spec.plain) html += pill("mood", "表情");
      else if (/^image:./.test(inner) && !spec.plain) {
        const src = spec.images ? photoSrc(spec.images, inner.slice(6).trim(), 240) : "";
        html += src ? `<img class="tpl-thumb" src="${esc(src)}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer">` : pill("image", "图片");
      } else if (inner.includes("|")) html += `<span class="tpl-pick" title="每次随机换成其中一个：${esc(inner.split("|").join("、"))}">${esc(inner.split("|")[0])}</span>`;
      else if (snippets.has(inner) && depth === 0) {
        // 自定义占位符：把它的内容就地展开预览（里面的占位符照这条提示的值填），只展开一层。
        const body = String(samples[inner] ?? "");
        html += body.trim()
          ? `<span class="tpl-snippet" title="{${esc(inner)}}">${part(previewOf(body, ctx, spec, 1).html)}</span>`
          : `<span class="tpl-empty">（{${esc(inner)}} 是空的）</span>`;
      } else if (allowed.has(inner) && !snippets.has(inner) && inner in samples) html += `<span class="tpl-var">${esc(samples[inner])}</span>`;
      else {
        html += `<mark class="tpl-bad">${esc(match[0])}</mark>`;
        bad.push(match[0]);
      }
    }
    html += esc(text.slice(last));
    if (spec.kind === "join" && !images && (valueAt(ctx.config, "join.images") || []).length) html += `\n${pill("image", "配图")}`;
    if (!text.trim() && depth === 0) html = `<span class="tpl-empty">空着：发的时候跳过这一种</span>`;
    const problems = bad.length
      ? h`${icon("warning")}<span>${[...new Set(bad)].join(" ")} ${bad.length > 1 ? "这几个" : "这个"}占位符在这里用不了${spec.plain ? "（这句插在提示中间，只认文字）" : "，会原样发出去"}。</span>`
      : "";
    return { html: raw(html), problems };
  }

  /** 编辑器现在的几种说法（含还没保存的）。 */
  const tplRead = (root) => $$("[data-tpl-input]", root).map((input) => input.value);

  /** 文案编辑器的上下文：配置表单里的看插件，自动回复的规则面板有自己的一份。 */
  const tplCtx = (root) => configs.get(root?.closest("[data-config]")?.dataset.config) || (root?.closest("[data-rule]") ? rules.ctx : null);

  function tplRepaint(input) {
    const root = input.closest("[data-tpl]");
    const ctx = tplCtx(root);
    const spec = ctx && tplSpec(ctx, root.dataset.tpl);
    if (!spec) return;
    const item = input.closest(".tpl-item");
    const { html, problems } = previewOf(input.value, ctx, spec);
    put($(".tpl-bubble", item), html);
    put($(".tpl-problems", item), problems);
  }

  /** 设置一改（验证方式、时限……），预览里的样例跟着变。 */
  function repaintPreviews(name) {
    for (const input of $$(`[data-config="${CSS.escape(name)}"] [data-tpl-input]`)) tplRepaint(input);
  }

  /** 整个编辑器重画一遍（加、删说法之后），焦点落到指定的那一种说法上。 */
  function tplRedraw(root, variants, focus = -1) {
    const ctx = tplCtx(root);
    const path = root.dataset.tpl;
    const holder = document.createElement("div");
    put(holder, templateBody(root.id, path, variants, ctx, templateOf(ctx, path)));
    const fresh = holder.firstElementChild;
    fresh.dataset.tplSaved = root.dataset.tplSaved;
    root.replaceWith(fresh);
    const target = focus >= 0 ? $$("[data-tpl-input]", fresh)[focus] : $("[data-tpl-add]", fresh);
    target?.focus({ preventScroll: focus < 0 });
    if (focus >= 0) target.setSelectionRange(target.value.length, target.value.length);
    return fresh;
  }

  /** 存下这个编辑器：和上次存的一样就不发请求。 */
  async function tplSave(root) {
    const form = root.closest("[data-config]");
    // 规则面板里的回复：随整条规则一起存。
    if (!form && root.closest("[data-rule]")) return ruleSave(root.closest("[data-rule]"));
    if (!form || root.getAttribute("aria-busy") === "true") return;
    const value = tplRead(root);
    if (JSON.stringify(value) === root.dataset.tplSaved) return;
    root.setAttribute("aria-busy", "true");
    const row = root.closest("[data-setting]");
    const first = $("[data-tpl-input]", root) || $("[data-tpl-add]", root);
    try {
      await configPost(form, "config", { path: root.dataset.tpl, value });
      root.dataset.tplSaved = JSON.stringify(value);
      if (first) clearFieldError(first);
      markModified(row, value);
      const ctx = configs.get(form.dataset.config);
      const spec = ctx && templateOf(ctx, root.dataset.tpl);
      const custom = spec?.kind === "custom";
      const label = custom ? `{${root.dataset.tpl.split(".").pop()}}` : spec?.title || (ctx && labelOf(ctx, root.dataset.tpl)) || root.dataset.tpl;
      snackbar(value.some((text) => text.trim()) ? `已保存「${label}」` : custom || spec?.plain ? `已保存：「${label}」现在是空的` : `已保存：「${label}」现在不发`);
      if (custom) {
        await freshPlugin(form.dataset.config).catch(() => null);
        repaintHolders(form.dataset.config);
        const entry = root.closest("details.entry");
        const first = value.find((text) => text.trim()) || "";
        if (entry) $("[data-entry-sub]", entry).textContent = first ? snippetBrief(first) : "还是空的";
      } else if (spec?.plain) {
        // 说法变了：别的编辑器里 {task} 这些的样例与芯片说明跟着换。
        await refreshDiff(form.dataset.config);
        repaintHolders(form.dataset.config);
      } else refreshDiff(form.dataset.config);
    } catch (error) {
      if (error.status === 401) return;
      if (first) fieldError(first, error.message);
      else report(error);
    } finally {
      root.removeAttribute("aria-busy");
      // 保存的工夫里又改了：再存一次。
      if (root.isConnected && JSON.stringify(tplRead(root)) !== root.dataset.tplSaved && !root.contains(document.activeElement)) void tplSave(root);
    }
  }

  /** 点芯片插入占位符：插在光标处（没点过哪一种说法就插进第一种），焦点回到输入框、光标落在插入之后。 */
  function tplInsert(button) {
    const root = button.closest("[data-tpl]");
    const inputs = $$("[data-tpl-input]", root);
    if (!inputs.length) {
      const fresh = tplRedraw(root, [button.dataset.tplInsert], 0);
      void tplSave(fresh);
      return;
    }
    const input = inputs[Number(root.dataset.tplActive) || 0] || inputs[0];
    const text = button.dataset.tplInsert;
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? start;
    input.focus({ preventScroll: true });
    input.setRangeText(text, start, end, "end");
    // 随机片段选中示例文字，直接打字就替换掉。
    if (text.includes("|")) input.setSelectionRange(start + 1, start + text.length - 1);
    tplRepaint(input);
    tplGrow(input);
  }

  /** 输入框随内容长高，免得一句三行的欢迎词要在两行高的框里滚着改。 */
  function tplGrow(input) {
    if (CSS.supports("field-sizing", "content")) return;
    input.style.height = "auto";
    input.style.height = `${input.scrollHeight + 2}px`;
  }

  /* ---------- 映射表：键由人定的表（每群覆盖、供应商、搜索后端……） ---------- */

  /** 一项的名字：群显示群名，私聊目标显示好友名，其余就是键本身。 */
  function entryLabel(spec, key) {
    if (spec.key === "name") return { title: key, sub: "" };
    if (spec.key === "word") return { title: `{${key}}`, sub: "" };
    const privateId = key.startsWith("private:") ? key.slice(8) : "";
    if (privateId) {
      const name = entryName("user", privateId);
      return { title: name || `私聊 ${privateId}`, sub: name ? `私聊 · ${privateId}` : "私聊" };
    }
    const name = entryName("group", key);
    return { title: name || `群 ${key}`, sub: name ? `群号 ${key}` : "" };
  }

  function mapEditor(path, key, value, spec, ctx) {
    const keys = Object.keys(value);
    const about = ctx.help[path] || helpFor(ctx, path);
    if (spec.key === "group" || spec.key === "target") void Promise.all([KINDS.group.roster.load(), spec.key === "target" ? KINDS.user.roster.load() : null]).then(paintEntries);
    const adders = spec.key === "target"
      ? [["group", "groups", "加一个群"], ["user", "person", "加一个私聊对象"]]
      : spec.key === "group" ? [["group", "groups", spec.add]] : [[spec.key === "word" ? "word" : "name", "add", spec.add]];
    // 这张表本身是上一层某一项里的字段（每群的自定义占位符）：可以整张删掉，沿用上层。
    const outer = mapOf(ctx, path);
    const inherit = outer?.field && outer.spec.optional
      ? h`<button class="icon-btn state field-reset legend-action" type="button" data-inherit="${path}"
          aria-label="不再单独设置 ${labelOf(ctx, path) || key}，沿用上层" title="不再单独设置，沿用上层">${icon("settings_backup_restore")}</button>`
      : "";
    return h`<fieldset class="config-group config-map${outer?.field ? " config-map-nested" : ""}" data-map="${path}" data-search="${`${path} ${about || ""} ${keys.join(" ")}`.toLowerCase()}">
      <legend class="config-legend">${legendName(ctx, path, key)}${keys.length ? h`<span class="legend-count num">${keys.length}</span>` : ""}${inherit}</legend>
      ${about ? h`<p class="config-about">${about}</p>` : ""}
      ${keys.length
        ? keys.map((item) => mapEntry(path, item, value[item], spec, ctx))
        : h`<p class="map-empty">${icon("info")}<span>${spec.empty}</span></p>`}
      <div class="map-actions">${adders.map(([kind, glyph, label]) =>
        h`<button class="btn btn-tonal" type="button" data-map-add="${path}" data-add-kind="${kind}">${icon(glyph)}${label}</button>`)}</div>
    </fieldset>`;
  }

  /** 自定义占位符在列表里的一句话：头一种说法，压成一行。 */
  const snippetBrief = (text) => {
    const line = String(text).replace(/\s+/g, " ").trim();
    return [...line].length > 40 ? `${[...line].slice(0, 40).join("")}…` : line;
  };

  function mapEntry(path, key, value, spec, ctx) {
    const full = `${path}.${key}`;
    if (spec.value === "template") {
      // 每一项本身就是一条提示模板（自定义占位符）：展开就是文案编辑器。
      const first = variantsOf(value).find((text) => text.trim()) || "";
      return h`<details class="memory entry" data-entry="${full}" data-entry-key="${key}" data-key-kind="word"${attr(openEntries.has(`${ctx.name}:${full}`), "open")}
          data-search="${`${full} ${key} ${variantsOf(value).join(" ")}`.toLowerCase()}">
        <summary class="state">
          <span class="item-leading" aria-hidden="true">${icon("data_object")}</span>
          <span class="item-content">
            <span class="item-title" data-entry-title><code>{${key}}</code></span>
            <span class="item-sub" data-entry-sub>${first ? snippetBrief(first) : "还是空的"}</span>
          </span>
          ${icon("chevron_right", "chevron")}
        </summary>
        <div class="memory-body entry-body">
          <div class="config-group">${configField(full, key, value, ctx)}</div>
          <div class="actions">
            <button class="btn btn-text btn-danger" type="button" data-entry-remove="${full}" data-entry-name="{${key}}">${icon("delete")}删除「{${key}}」</button>
          </div>
        </div>
      </details>`;
    }
    const fields = isTable(value) ? Object.entries(value).sort((a, b) => fieldOrder(a, b) || byOrder(ctx, full)(a, b)) : [];
    const missing = spec.optional
      ? Object.keys(spec.template).filter((field) => !(field in (value || {}))).sort((a, b) => byOrder(ctx, full)([a], [b]))
      : [];
    const { title, sub } = entryLabel(spec, key);
    const detail = spec.key === "name" ? value?.api_base || value?.base_url || "" : "";
    const count = spec.optional ? `${fields.length} 项单独设置` : "";
    const glyph = spec.key === "name" ? "key" : key.startsWith("private:") ? "person" : "groups";
    return h`<details class="memory entry" data-entry="${full}" data-entry-key="${key}" data-key-kind="${spec.key}"${attr(openEntries.has(`${ctx.name}:${full}`), "open")}
        data-search="${`${full} ${title} ${Object.keys(value || {}).join(" ")}`.toLowerCase()}">
      <summary class="state">
        <span class="item-leading" aria-hidden="true">${icon(glyph)}</span>
        <span class="item-content">
          <span class="item-title" data-entry-title>${title}</span>
          <span class="item-sub" data-entry-sub>${[sub, detail, count].filter(Boolean).join(" · ") || " "}</span>
        </span>
        ${icon("chevron_right", "chevron")}
      </summary>
      <div class="memory-body entry-body">
        ${fields.length ? h`<div class="config-group">${fields.map(([field, item]) => configField(`${full}.${field}`, field, item, ctx))}</div>` : ""}
        ${missing.length
          ? h`<div class="entry-missing">
              <p class="field-hint">${fields.length ? "其余沿用上层设置。" : "全部沿用上层设置。"}想单独设置哪一项，点它：</p>
              <div class="chips">${missing.map((field) => h`<button class="chip state" type="button" data-entry-field="${full}.${field}"
                aria-label="单独设置 ${labelOf(ctx, `${full}.${field}`) || field}">${icon("add")}${labelOf(ctx, `${full}.${field}`) || field}</button>`)}</div>
            </div>`
          : ""}
        <div class="actions">
          <button class="btn btn-text btn-danger" type="button" data-entry-remove="${full}" data-entry-name="${title}">${icon("delete")}删除「${title}」</button>
        </div>
      </div>
    </details>`;
  }

  /** 群名与好友名晚到：只改标题与副标题，不重画（重画会合上展开的项、冲掉输入）。 */
  function paintEntries() {
    for (const entry of $$("details.entry[data-key-kind]")) {
      const kind = entry.dataset.keyKind;
      if (kind === "name" || kind === "word") continue;
      const fieldset = entry.closest("[data-map]");
      const form = entry.closest("[data-config]");
      const ctx = configs.get(form?.dataset.config);
      const spec = ctx && fieldset && mapSpec(ctx, fieldset.dataset.map);
      if (!spec) continue;
      const { title, sub } = entryLabel(spec, entry.dataset.entryKey);
      $(":scope > summary [data-entry-title]", entry).textContent = title;
      const count = spec.optional ? `${$$(":scope > .entry-body > .config-group > :is([data-setting], [data-map])", entry).length} 项单独设置` : "";
      $(":scope > summary [data-entry-sub]", entry).textContent = [sub, count].filter(Boolean).join(" · ") || " ";
      // 只认这一项自己的删除键：项里还套着一张小表（每群的自定义占位符）时，那里面也有删除键。
      const remove = $(":scope > .entry-body > .actions [data-entry-remove]", entry);
      remove.dataset.entryName = title;
      remove.lastChild.textContent = `删除「${title}」`;
    }
  }

  /** 重画一整张映射表（加项、删项、补字段之后）。展开状态与焦点由调用方指定。 */
  async function rebuildMap(form, mapPath, focus = "") {
    const name = form.dataset.config;
    const plugin = await freshPlugin(name);
    const ctx = configs.get(name);
    const node = $(`[data-map="${CSS.escape(mapPath)}"]`, form);
    if (!node || !ctx) return;
    const spec = mapSpec(ctx, mapPath);
    const holder = document.createElement("div");
    put(holder, mapEditor(mapPath, mapPath.split(".").pop(), valueAt(plugin.config, mapPath) || {}, spec, ctx));
    const fresh = holder.firstElementChild;
    node.replaceWith(fresh);
    if (spec?.value === "template") repaintHolders(name);
    (focus ? $(focus, fresh) : null)?.focus({ preventScroll: false });
  }

  /** 新的一项叫什么：群与私聊对象从名册里选，其余的（供应商、搜索后端）起个名字。 */
  async function askEntryKeys(spec, kind, existing) {
    if (kind === "group" || kind === "user") {
      const picked = await openSheet({ selected: [], title: spec.noun, kind });
      if (!picked) return [];
      const keys = picked.map((id) => (kind === "user" ? `private:${id}` : id));
      const fresh = keys.filter((key) => !existing.includes(key));
      if (fresh.length < keys.length) snackbar(fresh.length ? "已经有单独设置的那几个跳过了。" : "选的都已经有单独设置了。");
      return fresh;
    }
    if (kind === "word") {
      const reserved = spec.reserved || [];
      const name = await promptName({
        title: spec.add,
        label: `${spec.noun}的名字`,
        hint: "提示里写 {名字} 就插进去，比如「群规」「下载地址」。最多 20 个字，不能有空格、花括号、竖线、冒号、点和引号。",
        taken: existing,
        mono: false,
        validate: (text) =>
          [...text].length > 20 ? "名字最多 20 个字。"
            : /[\s{}|:.'"]/.test(text) ? "名字里不能有空格、花括号、竖线、冒号、点和引号。"
              : reserved.includes(text) ? `{${text}} 是内置占位符，换个名字。` : "",
      });
      return name ? [name] : [];
    }
    const name = await promptName({
      title: spec.add,
      label: `${spec.noun}的名字`,
      hint: "只用字母、数字、下划线和连字符；房间模型写「名字/模型」时就按它来找。",
      suggest: (spec.suggest || []).filter((item) => !existing.includes(item)),
      taken: existing,
    });
    return name ? [name] : [];
  }

  async function addEntries(button) {
    const form = button.closest("[data-config]");
    const mapPath = button.dataset.mapAdd;
    const ctx = configs.get(form?.dataset.config);
    const spec = ctx && mapSpec(ctx, mapPath);
    if (!spec || button.getAttribute("aria-busy") === "true") return;
    const existing = $$(":scope > details.entry", button.closest("[data-map]")).map((entry) => entry.dataset.entryKey);
    const keys = await askEntryKeys(spec, button.dataset.addKind, existing);
    if (!keys.length) return;
    button.setAttribute("aria-busy", "true");
    const done = [];
    try {
      for (const key of keys) {
        await configPost(form, "config/insert", { path: `${mapPath}.${key}`, value: spec.template });
        done.push(key);
        openEntries.add(`${ctx.name}:${mapPath}.${key}`);
      }
      const said = done.length === 1 ? `已添加「${entryLabel(spec, done[0]).title}」` : `已添加 ${done.length} 个`;
      snackbar(spec.value === "template" ? `${said}，在下面写上它的内容。` : spec.optional ? `${said}，先照抄上层的设置；改哪项就是给它单独定。` : `${said}，把下面几项填上就能用。`);
    } catch (error) {
      report(error);
    } finally {
      button.removeAttribute("aria-busy");
      if (done.length) {
        const last = `[data-entry="${CSS.escape(`${mapPath}.${done[done.length - 1]}`)}"]`;
        await rebuildMap(form, mapPath, spec.value === "template" ? `${last} [data-tpl-input]` : `${last} > summary`).catch(report);
      }
    }
  }

  async function removeEntry(button) {
    const form = button.closest("[data-config]");
    const path = button.dataset.entryRemove;
    const mapPath = button.closest("[data-map]")?.dataset.map;
    const secret = $$("[data-secret='set']", button.closest("details")).length > 0;
    const ctx = configs.get(form?.dataset.config);
    const optional = ctx && mapPath && mapSpec(ctx, mapPath)?.optional;
    const ok = await confirmAction({
      title: `删除「${button.dataset.entryName}」？`,
      body: secret
        ? "这一项连同它的密钥一起删掉，无法撤销：密钥不会显示在页面上，删了只能重新填。"
        : optional
          ? "这一项的单独设置全部删掉，之后照上层的设置走。"
          : "这一项连同它的全部设置一起删掉。",
      confirm: "删除",
      danger: true,
    });
    if (!ok) return;
    try {
      const reply = await configPost(form, "config/remove", { path });
      openEntries.delete(`${form.dataset.config}:${path}`);
      snackbar(reply.message);
      await rebuildMap(form, mapPath, ".map-actions .btn");
    } catch (error) {
      report(error);
    }
  }

  /** 一项里的某个字段：「沿用上层」= 删掉它；点「补上」= 从模板加回来。 */
  async function entryField(button, remove) {
    const form = button.closest("[data-config]");
    const ctx = configs.get(form?.dataset.config);
    const path = remove ? button.dataset.inherit : button.dataset.entryField;
    const inside = ctx && mapOf(ctx, path);
    if (!inside?.field || button.getAttribute("aria-busy") === "true") return;
    // 字段本身是一张表（每群的自定义占位符）时，按钮在那张小表里；要重画的是装着这一项的外层表。
    const mapPath = inside.map;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = remove
        ? await configPost(form, "config/remove", { path })
        : await configPost(form, "config/insert", { path, value: inside.spec.template[inside.field] });
      openEntries.add(`${ctx.name}:${mapPath}.${inside.key}`);
      snackbar(remove ? `「${inside.field}」改回沿用上层。` : reply.message);
      const entry = `[data-entry="${CSS.escape(`${mapPath}.${inside.key}`)}"]`;
      await rebuildMap(form, mapPath, remove ? `${entry} [data-entry-field="${CSS.escape(path)}"]` : `${entry} [data-setting="${CSS.escape(path)}"] [data-path], ${entry} [data-setting="${CSS.escape(path)}"] .tags-add input`);
    } catch (error) {
      button.removeAttribute("aria-busy");
      report(error);
    }
  }

  /** 起名对话框：一个输入框加几枚建议。原生 dialog 管焦点与 Esc；名字不合规或重名就地说明。 */
  function promptName({ title, label, hint, suggest = [], taken = [], validate = null, mono = true }) {
    const dialog = $("#dialog");
    const opener = document.activeElement;
    put(
      dialog,
      h`<h2 class="dialog-title" id="dialog-title">${title}</h2>
        <form class="dialog-form" method="dialog" novalidate>
          <div class="field">
            <label class="field-label" for="dialog-name">${label}</label>
            <input class="input${mono ? " mono" : ""}" id="dialog-name" autocomplete="off" spellcheck="false" autocapitalize="off" enterkeyhint="done" aria-describedby="dialog-body">
            <span class="field-hint" id="dialog-body">${hint}</span>
          </div>
          ${suggest.length ? h`<div class="chips" role="group" aria-label="常用的名字">${suggest.map((item) => h`<button class="chip state" type="button" data-suggest="${item}">${item}</button>`)}</div>` : ""}
          <div class="actions actions-end">
            <button class="btn btn-text" type="button" value="cancel">取消</button>
            <button class="btn btn-filled" type="submit" value="confirm">添加</button>
          </div>
        </form>`
    );
    const input = $("#dialog-name", dialog);
    const check = () => {
      const name = input.value.trim();
      const shape = validate ? validate(name) : !/^[A-Za-z0-9_-]{1,40}$/.test(name) ? "只能用字母、数字、下划线和连字符，不超过 40 个字。" : "";
      const problem = !name ? "先起个名字。" : shape || (taken.includes(name) ? `「${name}」已经有了。` : "");
      if (problem) fieldError(input, problem);
      return problem ? "" : name;
    };
    return new Promise((resolve) => {
      let result = "";
      const click = (event) => {
        const chip = event.target.closest("[data-suggest]");
        if (chip) {
          input.value = chip.dataset.suggest;
          clearFieldError(input);
          input.focus();
          return;
        }
        if (event.target.closest('button[value="cancel"]')) dialog.close("cancel");
      };
      const submit = (event) => {
        event.preventDefault();
        event.stopPropagation();
        result = check();
        if (result) dialog.close("confirm");
        else input.focus();
      };
      const typing = () => clearFieldError(input);
      dialog.addEventListener("click", click);
      dialog.addEventListener("submit", submit);
      input.addEventListener("input", typing);
      dialog.addEventListener(
        "close",
        () => {
          dialog.removeEventListener("click", click);
          dialog.removeEventListener("submit", submit);
          if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
          resolve(dialog.returnValue === "confirm" ? result : "");
        },
        { once: true }
      );
      dialog.returnValue = "";
      dialog.showModal();
      input.focus();
    });
  }

  /** 配置项筛选：键名与说明都认；落在折起来的项里的命中会把那一项展开。 */
  function filterConfig(input) {
    const form = $(`[data-config="${CSS.escape(input.dataset.filter)}"]`);
    if (!form) return;
    const terms = input.value.normalize("NFKC").toLowerCase().split(/\s+/).filter(Boolean);
    const hit = (node) => terms.every((term) => (node.dataset.search || "").includes(term));
    // 筛选时命中的行不再相邻，拼成一张纸的圆角接不上：各自成片。
    form.toggleAttribute("data-filtering", terms.length > 0);
    // 由里往外：先判每一行，再判装着它们的项与小节——小节自己的名字命中时整节都留着。
    const groups = $$("fieldset[data-search], details.entry", form);
    for (const row of $$("[data-setting]", form)) {
      row.hidden = terms.length > 0 && !hit(row) && !groups.some((group) => group.contains(row) && hit(group));
    }
    for (const group of groups.reverse()) {
      const any = !terms.length || hit(group) || $$("[data-setting]", group).some((row) => !row.hidden);
      group.hidden = !any;
      if (terms.length && any && group.matches("details.entry") && !group.open) {
        group.open = true;
        group.dataset.autoOpen = "";
      } else if (!terms.length && group.dataset.autoOpen !== undefined) {
        group.open = false;
        delete group.dataset.autoOpen;
      }
    }
    // 筛选时只看配置项本身：小节说明、空表提示、添加按钮先收起来。
    for (const note of $$(".config-about, .map-empty, .map-actions, .entry-missing", form)) note.hidden = terms.length > 0;
    const count = $$("[data-setting]", form).filter((row) => !row.closest("[hidden]")).length;
    const status = $(`#${CSS.escape(input.id)}-status`);
    if (status) status.textContent = !terms.length ? "" : count ? `${count} 项匹配` : "没有匹配的配置项。换个关键词试试。";
    form.toggleAttribute("data-filtered", terms.length > 0);
  }

  async function togglePlugin(button) {
    if (button.disabled || button.getAttribute("aria-busy") === "true") return;
    const name = button.dataset.toggle;
    const next = button.getAttribute("aria-checked") !== "true";
    const all = $$(`[data-toggle="${CSS.escape(name)}"]`);
    for (const item of all) item.setAttribute("aria-busy", "true");
    try {
      const reply = await api(`/plugins/${encodeURIComponent(name)}/enabled`, { method: "POST", body: { on: next } });
      snackbar(reply.message);
      refreshLive(name);
      if (name === "autoreply") rulesSwitched(next);
      // 以后端为准：重新取一次列表，就地改开关与状态，不整页重画。
      const fresh = await api("/plugins").catch(() => null);
      if (fresh) plugins.data = fresh;
      const plugin = plugins.data?.plugins.find((p) => p.name === name) || { name, on: next, pending: false };
      if (!fresh) plugin.on = next;
      for (const item of $$(`[data-toggle="${CSS.escape(name)}"]`)) item.setAttribute("aria-checked", String(plugin.on));
      const row = $(`#plugin-list [data-plugin="${CSS.escape(name)}"] .item-link`);
      if (row) {
        $(".status", row)?.remove();
        if (plugin.pending) row.insertAdjacentHTML("beforeend", part(h`<span class="status status-warning">待重启</span>`));
      }
      for (const badge of $$(`[data-status-for="${CSS.escape(name)}"]`)) put(badge, statusBadge(plugin));
      if (plugins.data) {
        const counter = $("#plugin-count");
        if (counter) counter.textContent = plugins.summary();
        put($("#plugin-filters"), plugins.filters());
        const head = $("#plugin-head-summary");
        if (head) head.textContent = plugins.headline();
        // 正在按状态筛时，刚翻过的行可能已经不该出现：重画列表，焦点跟着走。
        if (plugins.status) {
          const inList = document.activeElement?.closest?.("#plugin-list");
          put($("#plugin-list"), plugins.rows());
          if (inList) {
            const still = $(`#plugin-list [data-plugin="${CSS.escape(name)}"] [data-toggle]`);
            if (still) still.focus({ preventScroll: true });
            else $("[data-status-filter]")?.focus({ preventScroll: true });
          }
        }
      }
    } catch (error) {
      report(error);
    } finally {
      for (const item of $$(`[data-toggle="${CSS.escape(name)}"]`)) item.removeAttribute("aria-busy");
    }
  }

  async function resetPlugin(button) {
    const name = button.dataset.reset;
    const ok = await confirmAction({
      title: `恢复「${button.dataset.display || name}」的默认参数？`,
      body: "当前的全部配置值会被默认值覆盖，插件的启用状态保持不变。这一步无法撤销。",
      confirm: "恢复默认",
      danger: true,
    });
    if (!ok) return;
    try {
      const reply = await api(`/plugins/${encodeURIComponent(name)}/reset`, { method: "POST", body: {} });
      snackbar(reply.message);
      $(`[data-config="${CSS.escape(name)}"]`)?.remove();
      if (wide.matches && $("#plugin-detail")) await plugins.swapDetail(name);
      else await render({ quiet: true });
      $(`[data-reset="${CSS.escape(name)}"]`)?.focus({ preventScroll: true });
    } catch (error) {
      report(error);
    }
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text);
      snackbar(`已复制 ${text}`);
    } catch {
      snackbar("浏览器没有给剪贴板权限；可以长按或拖选文字复制。", "error");
    }
  }

  /* ==================== §8b 自动回复的规则面板 ====================
     规则不在配置里（data/autoreply/rules.json），所以不画成配置表单：每条规则一张可展开的卡，
     改完即存——整条规则一起存，存不上就留着草稿、在卡里说清哪里不行，删了能撤销。
     触发词、群名单、回复分别复用列表编辑器、名单编辑器与文案编辑器，只是写入换成 emit：
     值交给这条规则，由 ruleSave 一并提交。上方「试一句」随打随试，防误触从这里开始。 */

  const rules = {
    data: null,
    /** 文案编辑器的上下文（占位符、样例、图库）。 */
    ctx: null,
    open: new Set(),
    /** 展开了「更多设置」的规则：重画时照旧展开。 */
    more: new Set(),
    tryTimer: 0,
    trySeq: 0,
    mounting: 0,
  };

  const RULE_EXTRAS = [
    { insert: "{face:14}", label: "QQ 表情", hint: "{face:ID}：插一个 QQ 小黄脸，ID 是表情编号（14 是微笑）" },
    { insert: "{你好|嗨}", label: "随机片段", hint: "{甲|乙|丙}：每次随机换成其中一个" },
  ];

  /** 详情里的那一节：先占个位，数据到了再画（规则在另一条接口上）。 */
  function rulesSection(sub) {
    clearTimeout(rules.mounting);
    rules.mounting = setTimeout(() => void rulesMount(), 0);
    return h`<section class="section rules" id="rules-panel" aria-labelledby="rules-title" aria-busy="true">
      <div class="section-head">
        ${raw(`<${sub} class="section-title" id="rules-title">回复规则</${sub}>`)}
        <span class="section-meta" id="rules-count">正在读取…</span>
      </div>
      <div id="rules-body"><div class="progress" role="progressbar" aria-label="正在读取规则"></div></div>
    </section>`;
  }

  async function rulesMount({ focus = "" } = {}) {
    const panel = $("#rules-panel");
    if (!panel) return;
    try {
      const data = await api("/autoreply");
      if (!panel.isConnected) return;
      rules.data = data;
      rules.ctx = {
        name: "autoreply",
        maps: {},
        help: {},
        labels: {},
        config: {},
        templates: {
          placeholders: data.placeholders.map((holder) => ({ ...holder, kinds: ["reply"] })),
          samples: data.samples,
          extras: RULE_EXTRAS,
          fallback: { kind: "reply", about: "回复的几种说法", images: "autoreply", silent: "没有文字回复。" },
        },
      };
      rulesPaint();
      void KINDS.group.roster.load().then(() => {
        rulesGroupOptions();
        for (const entry of $$("[data-rule]")) rulePaintSummary(entry);
      });
      if (focus) $(focus)?.focus({ preventScroll: false });
    } catch (error) {
      if (error.status === 401) return;
      put($("#rules-body"), emptyState(`规则没读出来：${error.message}`, "error"));
      $("#rules-count").textContent = "";
    } finally {
      panel.removeAttribute("aria-busy");
    }
  }

  function rulesPaint() {
    const { rules: list, trash } = rules.data;
    $("#rules-count").textContent = list.length ? `${list.length} 条 · 改完即保存` : "";
    put($("#rules-body"), h`
      <div id="rules-off">${rulesOff()}</div>
      <div class="card rules-try">
        <div class="field-text">
          <label class="field-label" for="rules-try-input">试一句</label>
          <span class="field-hint" id="rules-try-hint">输入群友可能发的话，看会不会回、由哪条回；不回也会说清是哪一步拦下的。加规则前后都试试，误触在这里就看得出来。</span>
        </div>
        <div class="rules-try-row">
          <input class="input" id="rules-try-input" type="text" enterkeyhint="search" autocomplete="off" spellcheck="false"
            placeholder="比如：请问在哪下载" aria-describedby="rules-try-hint rules-try-result">
          <span class="select"><select id="rules-try-group" aria-label="在哪儿说这句">${rulesGroupChoices()}</select>${icon("keyboard_arrow_down")}</span>
        </div>
        <div class="rules-try-result" id="rules-try-result" role="status"></div>
      </div>
      <div class="rules-tools">
        ${list.length >= 6
          ? h`<div class="search">${icon("search")}
              <input class="input" id="rules-search" type="search" placeholder="在 ${list.length} 条规则里找" aria-label="筛选规则"
                aria-describedby="rules-search-status" autocomplete="off" spellcheck="false" enterkeyhint="search">
            </div>`
          : ""}
        <button class="btn btn-filled" type="button" data-rule-new>${icon("add")}新建规则</button>
      </div>
      <p class="section-meta" id="rules-search-status" role="status"></p>
      <div class="rules-list" id="rules-list">
        ${list.length
          ? list.map(ruleEntry)
          : emptyState("还没有规则。点「新建规则」；群管理员也可以在群里发「自动回复 添加 下载 安装包在群文件里～」。", "quickreply")}
      </div>
      ${trash.length ? rulesTrash(trash) : ""}`);
  }

  /** 插件总开关关着时，规则面板顶上的一行：规则加得再好也不会回，这一点不能藏在下面的「现在的情况」里。 */
  function rulesOff() {
    if (rules.data?.enabled !== false) return "";
    return h`<div class="rules-off" role="note">
      ${icon("warning-fill")}
      <span class="rules-off-text"><strong>自动回复还关着。</strong>下面的规则都在，但群里问到也不会回。</span>
      <button class="btn btn-filled" type="button" data-rules-enable>${icon("power_settings_new")}打开</button>
    </div>`;
  }

  /** 总开关变了（上面的开关、或这一行的「打开」）：横幅跟着收起或出现，「试一句」重新判一次。 */
  function rulesSwitched(on) {
    if (!rules.data) return;
    rules.data.enabled = on;
    const slot = $("#rules-off");
    if (slot) put(slot, rulesOff());
    if ($("#rules-try-input")?.value.trim()) ruleTry();
  }

  function rulesEnable(button) {
    const master = $('.switch[data-toggle="autoreply"]');
    if (!master || master.getAttribute("aria-checked") === "true") return rulesSwitched(true);
    button.setAttribute("aria-busy", "true");
    void togglePlugin(master).then(() => {
      if (master.getAttribute("aria-checked") === "true") master.focus({ preventScroll: true });
      else button.removeAttribute("aria-busy");
    });
  }

  function rulesTrash(trash) {
    return h`<details class="memory rules-trash" id="rules-trash">
      <summary class="state">
        <span class="item-leading" aria-hidden="true">${icon("delete")}</span>
        <span class="item-content"><span class="item-title">最近删除</span><span class="item-sub">留最近 ${trash.length} 条，可以找回来</span></span>
        ${icon("chevron_right", "chevron")}
      </summary>
      <ul class="memory-body rules-trash-list">${trash.map((item) => h`<li class="rules-trash-row">
        <span class="item-content"><span class="item-title">#${item.id} ${item.title}</span>
          <span class="item-sub">${ago(item.at)}${item.group ? `在群 ${entryName("group", item.group) || item.group} 删的` : "删的"}</span></span>
        <button class="btn btn-tonal" type="button" data-rule-restore="${item.id}">${icon("undo")}找回</button>
      </li>`)}</ul>
    </details>`;
  }

  /** 「试一句」在哪儿说：只看共用规则的「别的群」、各个群（有单独规则的排前面）、私聊。 */
  function rulesGroupChoices(current = "*") {
    const scoped = new Set((rules.data?.rules || []).flatMap((rule) => rule.groups));
    const known = KINDS.group.roster.list.map((entry) => entry.id);
    const ids = [...new Set([...scoped, ...known])].sort((a, b) => scoped.has(b) - scoped.has(a));
    const option = (value, label) => h`<option value="${value}"${attr(value === current, "selected")}>${label}</option>`;
    return [
      option("*", "别的群（只看共用规则）"),
      ...ids.map((id) => option(id, `${entryName("group", id) || `群 ${id}`}${scoped.has(id) ? " · 有单独规则" : ""}`)),
      option("", "私聊"),
    ];
  }

  function rulesGroupOptions() {
    const select = $("#rules-try-group");
    if (select) put(select, rulesGroupChoices(select.value));
  }

  const modeOf = (code) => rules.data?.modes.find((mode) => mode.code === code) || { code, label: code, explain: "" };

  /** 卡片标题：触发词一个个排开；正则原样等宽显示。 */
  const ruleTitle = (rule) =>
    rule.keywords.length
      ? h`${rule.keywords.slice(0, 4).map((word) => h`<span class="rule-word${rule.mode === "regex" ? " mono" : ""}">${word}</span>`)}${rule.keywords.length > 4 ? h`<span class="rule-more">等 ${rule.keywords.length} 个</span>` : ""}`
      : h`<span class="rule-word rule-word-none">还没有触发词</span>`;

  function ruleWhere(rule) {
    if (rule.groups.length === 1) return entryName("group", rule.groups[0]) || `群 ${rule.groups[0]}`;
    if (rule.groups.length > 1) return `${rule.groups.length} 个群`;
    return rule.exclude.length ? `所有群（${rule.exclude.length} 个群不回）` : "所有群";
  }

  function ruleSub(rule) {
    const texts = rule.replies.filter((reply) => typeof reply.text === "string").length;
    const forwards = rule.replies.length - texts;
    const bits = [`#${rule.id}`, modeOf(rule.mode).label, ruleWhere(rule)];
    if (rule.replies.length > 1) bits.push(`${rule.replies.length} 种回复`);
    else if (forwards) bits.push("合并转发");
    bits.push(rule.hits ? `回过 ${num(rule.hits)} 次` : "还没回过");
    return bits.join(" · ");
  }

  /** 「更多设置」收起时的一行：改过的几项照实说，都没改就列出里面有什么。 */
  function ruleMoreSub(rule) {
    const bits = [];
    if (rule.exclude?.length) bits.push(`${rule.exclude.length} 个群不回`);
    if (rule.private) bits.push("私聊也回");
    if (rule.forward) bits.push("合并转发");
    if (rule.cooldown_seconds != null) bits.push(rule.cooldown_seconds ? `冷却 ${secondsText(rule.cooldown_seconds) || `${rule.cooldown_seconds} 秒`}` : "不冷却");
    return bits.length ? bits.join(" · ") : `${rule.groups.length ? "" : "停用的群、私聊、"}合并转发、冷却`;
  }

  const ruleFlags = (rule) =>
    h`${!rule.enabled ? h`<span class="status">已停用</span>` : ""}${rule.at_only ? h`<span class="rule-flag" title="只在 @ 机器人时回">${icon("alternate_email")}<span class="visually-hidden">只在 @ 时回</span></span>` : ""}${rule.forward || rule.replies.some((reply) => reply.forward) ? h`<span class="rule-flag" title="以合并转发发出">${icon("forward")}<span class="visually-hidden">合并转发</span></span>` : ""}`;

  const ruleSearchText = (rule) =>
    [`#${rule.id}`, ...rule.keywords, ...(rule.briefs || []), ...rule.groups.map((id) => `${id} ${entryName("group", id)}`)].join(" ").normalize("NFKC").toLowerCase();

  function ruleMeta(rule) {
    const bits = [rule.hits ? `回过 ${num(rule.hits)} 次，最近${ago(rule.last_hit)}` : "还没回过"];
    if (rule.author) bits.push(rule.author === "控制台" ? "在控制台添加" : `由 ${rule.author} 添加`);
    if (rule.created_at) bits.push(new Date(rule.created_at * 1000).toLocaleDateString("zh-CN"));
    return bits.join(" · ");
  }

  /** 一条规则的卡。存下来的那份放在 data-rule-saved 里：比对「改没改」、拼请求都从它出发。 */
  function ruleEntry(rule) {
    const id = `rule-${rule.id}`;
    const texts = rule.replies.filter((reply) => typeof reply.text === "string").map((reply) => reply.text);
    const forwards = rule.replies.filter((reply) => Array.isArray(reply.forward)).map((reply) => reply.forward);
    const defaults = rules.data.defaults;
    const mode = modeOf(rule.mode);
    const shared = !rule.groups.length;
    const toggle = (field, label, hint, on) => h`<div class="toggle-row setting">
        <div class="field-text"><span class="field-label" id="${id}-${field}">${label}</span>${hint ? h`<span class="field-hint" id="${id}-${field}-hint">${hint}</span>` : ""}</div>
        <button class="switch" type="button" role="switch" data-rule-switch="${field}" aria-checked="${bool(on)}" aria-labelledby="${id}-${field}"${hint ? h` aria-describedby="${id}-${field}-hint"` : ""}></button>
      </div>`;
    const head = (field, label, hint) => h`<div class="field-text">
        <span class="field-label" id="${id}-${field}-label">${label}</span>${hint ? h`<span class="field-hint" id="${id}-${field}-hint">${hint}</span>` : ""}
      </div>`;
    return h`<details class="memory rule" data-rule="${rule.id}" data-rule-saved="${JSON.stringify(rule)}" data-forwards="${JSON.stringify(forwards)}"
        data-search="${ruleSearchText(rule)}"${attr(rules.open.has(rule.id), "open")}${attr(!rule.enabled, "data-off")}>
      <summary class="state">
        <span class="item-leading" aria-hidden="true">${icon("quickreply")}</span>
        <span class="item-content">
          <span class="item-title rule-title" data-rule-title>${ruleTitle(rule)}</span>
          <span class="item-sub" data-rule-sub>${ruleSub(rule)}</span>
        </span>
        <span class="rule-flags"><span class="mod-tag" data-rule-unsaved hidden>没存上</span><span data-rule-flags>${ruleFlags(rule)}</span></span>
        ${icon("chevron_right", "chevron")}
      </summary>
      <div class="memory-body rule-body">
        <div class="config-group">
          ${toggle("enabled", "启用这条规则", "关掉就不回，规则还留着。", rule.enabled)}
          <div class="setting">${listEditor({
            id: `${id}-kw`, type: "text", items: rule.keywords, mode: "emit", title: "触发词", describedby: `${id}-kw-hint`,
            head: head("kw", "触发词", keywordHint(rule.mode)),
          })}</div>
          <div class="setting">
            ${head("mode", "怎么比", "")}
            <fieldset class="segmented segmented-fill rule-modes" aria-labelledby="${id}-mode-label" aria-describedby="${id}-mode-explain">
              ${rules.data.modes.map((item) => h`<label class="segment"><input type="radio" name="${id}-mode" value="${item.code}"${attr(item.code === rule.mode, "checked")}>${item.label}</label>`)}
            </fieldset>
            <p class="field-hint rule-explain" id="${id}-mode-explain">${mode.explain}${rule.mode === "exact" ? "。最不会误触" : rule.mode === "contains" ? "。触发词至少两个字" : ""}。</p>
          </div>
          <div class="setting tpl-row">
            ${head("reply", "回复", "几种说法每次随机一种。可以插图、表情、@ 提问的人；合并转发要在群里引用一条再发「自动回复 加说法 编号」。")}
            <ul class="rule-forwards" aria-label="合并转发回复">${forwards.map(forwardCard)}</ul>
            ${templateBody(`${id}-tpl`, `rules.${rule.id}`, texts, rules.ctx, templateOf(rules.ctx, `rules.${rule.id}`))}
          </div>
          <div class="setting">${picker({
            id: `${id}-groups`, kind: "group", mode: "emit", ids: rule.groups, role: "rule-groups", title: "只在这些群回",
            label: h`<span>在哪些群回</span><span class="field-hint">留空 = 所有群共用；加了群就只在这些群回。</span>`,
          })}</div>
          ${toggle("at_only", "只在 @ 机器人时回", "最稳的防误触：没 @ 就当没听见。", rule.at_only)}
          <details class="rule-more"${attr(rules.more.has(rule.id), "open")}>
            <summary class="state">
              <span class="item-content"><span class="item-title">更多设置</span><span class="item-sub" data-rule-more-sub>${ruleMoreSub(rule)}</span></span>
              ${icon("chevron_right", "chevron")}
            </summary>
            <div class="rule-more-body">
              ${shared
                ? h`<div class="setting">${picker({
                    id: `${id}-exclude`, kind: "group", mode: "emit", ids: rule.exclude, role: "rule-exclude", title: "这些群不回",
                    label: h`<span>这些群不回</span><span class="field-hint">共用规则在某几个群停用；群管理员在群里发「自动回复 停用 ${rule.id}」也会记到这里。</span>`,
                  })}</div>
                  ${toggle("private", "私聊里也回", "有人私聊机器人说到触发词时也回。", rule.private)}`
                : ""}
              ${toggle("forward", "折成合并转发", "长内容不刷屏；回复里单独一行写 --- 分成几条。", rule.forward)}
              <div class="field setting" data-row="short">
                <div class="field-text">
                  <label class="field-label" for="${id}-cooldown">冷却（秒）</label>
                  <span class="field-hint" id="${id}-cooldown-hint">同一个群里隔多久才回第二次；留空照全局（${defaults.cooldown_seconds} 秒），0 = 不冷却。同一个人 ${defaults.user_cooldown_seconds} 秒内只回一次、每群每分钟最多 ${defaults.per_minute} 次，照全局。</span>
                </div>
                <span class="input-unit">
                  <input class="input num" id="${id}-cooldown" data-rule-cooldown inputmode="numeric" spellcheck="false" autocomplete="off"
                    value="${rule.cooldown_seconds ?? ""}" placeholder="${defaults.cooldown_seconds}" aria-describedby="${id}-cooldown-hint ${id}-cooldown-unit">
                  <span class="unit-text num" id="${id}-cooldown-unit" data-seconds-for="${id}-cooldown">${secondsText(rule.cooldown_seconds ?? "")}</span>
                </span>
              </div>
            </div>
          </details>
        </div>
        <p class="field-error rule-error" id="${id}-error" role="alert" hidden></p>
        <div class="rule-foot">
          <span class="rule-meta" data-rule-meta>${ruleMeta(rule)}</span>
          <button class="btn btn-text btn-danger" type="button" data-rule-delete="${rule.id}">${icon("delete")}删除这条</button>
        </div>
      </div>
    </details>`;
  }

  /** 一整条合并转发：列出前几条，可以整条删掉（加一条要在群里引用后发指令）。 */
  function forwardCard(nodes, index) {
    const shown = nodes.slice(0, 4);
    const brief = (text) => {
      const plain = String(text).replace(/\{image:[^}]*\}/g, "〔图〕").replace(/\s+/g, " ").trim();
      return [...plain].length > 48 ? `${[...plain].slice(0, 48).join("")}…` : plain;
    };
    return h`<li class="rule-forward" data-forward-index="${index}">
      <div class="rule-forward-head">
        <span class="rule-forward-title">${icon("forward")}合并转发 · ${nodes.length} 条</span>
        <button class="icon-btn state" type="button" data-forward-remove="${index}" aria-label="删掉这条合并转发（${nodes.length} 条）">${icon("delete")}</button>
      </div>
      <ol class="rule-forward-nodes">${shown.map((node) => h`<li><span class="rule-forward-name">${node.name || node.user_id || "机器人"}</span>${brief(node.text)}</li>`)}</ol>
      ${nodes.length > shown.length ? h`<p class="field-hint">还有 ${nodes.length - shown.length} 条</p>` : ""}
    </li>`;
  }

  /** 触发词那一格的说明随比法变：正则对原文比，其余先折叠再比。 */
  const keywordHint = (mode) =>
    mode === "regex"
      ? "每一项是一条正则，任一对上就回；对消息原文比、不分大小写。可以照习惯写成 /…/。"
      : "任一对上就回。比之前去掉空格和标点、全角转半角、不分大小写；以指令前缀开头的消息只认同样带前缀的触发词。";

  /** 卡片上现在的样子拼成一条规则。 */
  function ruleRead(entry) {
    const saved = JSON.parse(entry.dataset.ruleSaved);
    const on = (field) => $(`[data-rule-switch="${field}"]`, entry)?.getAttribute("aria-checked") === "true";
    const forwards = JSON.parse(entry.dataset.forwards || "[]");
    const texts = tplRead($("[data-tpl]", entry));
    const exclude = $('.picker[data-role="rule-exclude"]', entry);
    const cooldown = $("[data-rule-cooldown]", entry).value.trim();
    return {
      ...saved,
      enabled: on("enabled"),
      keywords: readList($(".tags", entry)),
      mode: $(".rule-modes input:checked", entry)?.value || saved.mode,
      replies: [...forwards.map((forward) => ({ forward })), ...texts.map((text) => ({ text }))],
      groups: readIds($('.picker[data-role="rule-groups"]', entry)),
      exclude: exclude ? readIds(exclude) : [],
      private: $('[data-rule-switch="private"]', entry) ? on("private") : false,
      at_only: on("at_only"),
      forward: on("forward"),
      cooldown_seconds: cooldown === "" ? null : Number(cooldown.normalize("NFKC")),
    };
  }

  /** 和存下来的那份比，只看人能改的那几项。 */
  const RULE_FIELDS = ["enabled", "keywords", "mode", "replies", "groups", "exclude", "private", "at_only", "forward", "cooldown_seconds"];
  const ruleSame = (a, b) => RULE_FIELDS.every((field) => same(a[field] ?? null, b[field] ?? null));

  function ruleProblem(entry, message) {
    const note = $(".rule-error", entry);
    note.hidden = !message;
    put(note, message ? h`${icon("error-fill")}<span>没存上：${message}</span>` : "");
    entry.toggleAttribute("data-unsaved", !!message);
    $("[data-rule-unsaved]", entry).hidden = !message;
  }

  /** 存一条规则。存的工夫里又改了，等这次回来再补存一次；存不上就留着草稿、说明原因。 */
  async function ruleSave(entry, { said = "", undo = null } = {}) {
    if (!entry?.isConnected) return;
    if (entry.dataset.saving) {
      entry.pending = { said, undo };
      return;
    }
    const cooldown = $("[data-rule-cooldown]", entry);
    const draft = ruleRead(entry);
    if (draft.cooldown_seconds !== null && !(Number.isInteger(draft.cooldown_seconds) && draft.cooldown_seconds >= 0)) {
      fieldError(cooldown, "冷却写一个不小于 0 的整数（秒），或者留空照全局。");
      return;
    }
    clearFieldError(cooldown);
    if (ruleSame(draft, JSON.parse(entry.dataset.ruleSaved)) && !entry.hasAttribute("data-unsaved")) return;
    entry.dataset.saving = "1";
    entry.setAttribute("aria-busy", "true");
    try {
      const reply = await api("/autoreply/rules", { method: "POST", body: draft });
      ruleProblem(entry, "");
      ruleAdopt(entry, reply.rule);
      snackbar(said ? `${said}，已保存。` : reply.message, "info", undo);
    } catch (error) {
      if (error.status === 401) return;
      ruleProblem(entry, error.message);
    } finally {
      delete entry.dataset.saving;
      entry.removeAttribute("aria-busy");
      const later = entry.pending;
      entry.pending = null;
      if (later) void ruleSave(entry, later);
    }
  }

  /** 存好的规则写回卡片：基线、标题、副标题、标记、统计。控件本身不重画，免得冲掉焦点与光标。 */
  function ruleAdopt(entry, rule) {
    entry.dataset.ruleSaved = JSON.stringify(rule);
    const list = rules.data.rules;
    const index = list.findIndex((item) => item.id === rule.id);
    if (index >= 0) list[index] = rule;
    entry.toggleAttribute("data-off", !rule.enabled);
    entry.dataset.search = ruleSearchText(rule);
    rulePaintSummary(entry);
    const root = $("[data-tpl]", entry);
    if (root) root.dataset.tplSaved = JSON.stringify(rule.replies.filter((reply) => typeof reply.text === "string").map((reply) => reply.text));
    // 只在本群用的规则不需要「这些群不回」：范围一变，那两格要出现或收起。
    const shared = !rule.groups.length;
    if (shared !== !!$('.picker[data-role="rule-exclude"]', entry)) ruleRedraw(entry, rule);
  }

  function rulePaintSummary(entry) {
    const rule = JSON.parse(entry.dataset.ruleSaved);
    put($("[data-rule-title]", entry), ruleTitle(rule));
    $("[data-rule-sub]", entry).textContent = ruleSub(rule);
    put($("[data-rule-flags]", entry), ruleFlags(rule));
    const meta = $("[data-rule-meta]", entry);
    if (meta) meta.textContent = ruleMeta(rule);
    const more = $("[data-rule-more-sub]", entry);
    if (more) more.textContent = ruleMoreSub(rule);
  }

  /** 整张卡重画（范围变了、找回来了）：展开状态照旧，焦点尽量落回原处。 */
  function ruleRedraw(entry, rule) {
    const focused = document.activeElement;
    const selector = focused?.dataset?.ruleSwitch ? `[data-rule-switch="${focused.dataset.ruleSwitch}"]` : focused?.id ? `#${CSS.escape(focused.id)}` : "";
    const holder = document.createElement("div");
    put(holder, ruleEntry(rule));
    const fresh = holder.firstElementChild;
    fresh.open = entry.open;
    entry.replaceWith(fresh);
    if (selector && entry.contains(focused)) $(selector, fresh)?.focus({ preventScroll: true });
    return fresh;
  }

  async function ruleSwitch(button) {
    const entry = button.closest("[data-rule]");
    const on = button.getAttribute("aria-checked") !== "true";
    button.setAttribute("aria-checked", bool(on));
    await ruleSave(entry);
  }

  function ruleModePick(input) {
    const entry = input.closest("[data-rule]");
    const mode = modeOf(input.value);
    $(".rule-explain", entry).textContent = `${mode.explain}${mode.code === "exact" ? "。最不会误触" : mode.code === "contains" ? "。触发词至少两个字" : ""}。`;
    const hint = $(`#${CSS.escape(entry.querySelector(".tags")?.getAttribute("aria-describedby") || "")}`);
    if (hint) hint.textContent = keywordHint(mode.code);
    $(".tags", entry)?.classList.toggle("tags-regex", mode.code === "regex");
    void ruleSave(entry);
  }

  async function forwardRemove(button) {
    const entry = button.closest("[data-rule]");
    const index = Number(button.dataset.forwardRemove);
    const forwards = JSON.parse(entry.dataset.forwards || "[]");
    const [gone] = forwards.splice(index, 1);
    const paint = (list) => {
      entry.dataset.forwards = JSON.stringify(list);
      put($(".rule-forwards", entry), list.map(forwardCard));
    };
    paint(forwards);
    ($("[data-forward-remove]", entry) || $("[data-tpl-add]", entry))?.focus({ preventScroll: true });
    await ruleSave(entry, {
      said: `删掉了一条合并转发（${gone.length} 条）`,
      undo: {
        label: "撤销",
        run: () => {
          const back = JSON.parse(entry.dataset.forwards || "[]");
          back.splice(index, 0, gone);
          paint(back);
          void ruleSave(entry, { said: "合并转发放回去了" });
        },
      },
    });
  }

  async function ruleDelete(button) {
    const entry = button.closest("[data-rule]");
    const id = Number(entry.dataset.rule);
    if (button.getAttribute("aria-busy") === "true") return;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = await api(`/autoreply/rules/${id}/delete`, { method: "POST", body: {} });
      const entries = $$("[data-rule]");
      const next = entries[entries.indexOf(entry) + 1] || entries[entries.indexOf(entry) - 1];
      rules.open.delete(id);
      await rulesMount();
      ($(`[data-rule="${next?.dataset.rule}"] > summary`) || $("[data-rule-new]"))?.focus({ preventScroll: true });
      snackbar(reply.message, "info", { label: "撤销", run: () => void ruleRestore(id) });
    } catch (error) {
      button.removeAttribute("aria-busy");
      report(error);
    }
  }

  async function ruleRestore(id) {
    try {
      const reply = await api("/autoreply/restore", { method: "POST", body: { id } });
      rules.open.add(reply.rule.id);
      await rulesMount({ focus: `[data-rule="${reply.rule.id}"] > summary` });
      snackbar(reply.message);
    } catch (error) {
      report(error);
    }
  }

  /** 「试一句」：停手 300 毫秒再问，结果里带一枚「看这条」直达那张卡。 */
  function ruleTry() {
    clearTimeout(rules.tryTimer);
    rules.tryTimer = setTimeout(async () => {
      const input = $("#rules-try-input");
      const out = $("#rules-try-result");
      if (!input || !out) return;
      const text = input.value.trim();
      for (const entry of $$("[data-rule][data-hit]")) entry.removeAttribute("data-hit");
      if (!text) {
        put(out, "");
        return;
      }
      const seq = ++rules.trySeq;
      try {
        const group = $("#rules-try-group").value;
        const reply = await api("/autoreply/test", { method: "POST", body: { text, group } });
        if (seq !== rules.trySeq) return;
        const hit = reply.hit && $(`[data-rule="${reply.hit.id}"]`);
        if (hit) hit.dataset.hit = "";
        const tone = reply.tone || (reply.hit ? "ok" : "info");
        const glyph = { ok: "check_circle-fill", warn: "warning-fill", info: "info" }[tone] || "info";
        const lines = [...(reply.blocks || []), ...(reply.notes || [])];
        out.dataset.tone = tone;
        put(out, h`<p class="rules-try-line">${icon(glyph)}<span>${reply.message}</span>${hit
            ? h`<button class="btn btn-text" type="button" data-rule-show="${reply.hit.id}">看 #${reply.hit.id}</button>`
            : ""}</p>
          ${lines.length ? h`<ul class="rules-try-notes">${lines.map((line, index) => h`<li${attr(index < (reply.blocks || []).length, "data-block")}>${line}</li>`)}</ul>` : ""}`);
      } catch (error) {
        if (seq === rules.trySeq && error.status !== 401) put(out, h`<p class="rules-try-line">${icon("error")}<span>${error.message}</span></p>`);
      }
    }, 300);
  }

  function ruleShow(id) {
    const entry = $(`[data-rule="${CSS.escape(String(id))}"]`);
    if (!entry) return;
    entry.hidden = false;
    entry.open = true;
    rules.open.add(Number(id));
    $(":scope > summary", entry).focus({ preventScroll: true });
    entry.scrollIntoView({ block: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  }

  function rulesFilter(input) {
    const terms = input.value.normalize("NFKC").toLowerCase().split(/\s+/).filter(Boolean);
    let shown = 0;
    for (const entry of $$("[data-rule]")) {
      const hit = terms.every((term) => entry.dataset.search.includes(term));
      entry.hidden = !hit;
      shown += hit;
    }
    $("#rules-search-status").textContent = terms.length ? (shown ? `${shown} 条匹配` : "没有匹配的规则。换个词试试。") : "";
  }

  /** 新建：先问触发词和第一句回复（加上比法与范围），建好再展开细调。 */
  function ruleNew() {
    const dialog = $("#dialog");
    const opener = document.activeElement;
    const defaults = rules.data.defaults;
    const explain = (code) => `${modeOf(code).explain}。`;
    const wordsHint = (code) =>
      code === "regex" ? "写一条正则（里面的 | 是「或」，不会拆开），也可以写成 /…/；要几条就建好后在卡片里加。" : "几个触发词用 | 隔开，任一对上就回；写成 /…/ 就按正则比。";
    put(dialog, h`<h2 class="dialog-title" id="dialog-title">新建规则</h2>
      <form class="dialog-form" method="dialog" novalidate>
        <div class="field">
          <label class="field-label" for="rule-new-keywords">触发词</label>
          <input class="input" id="rule-new-keywords" autocomplete="off" spellcheck="false" enterkeyhint="next" placeholder="下载 | 安装包" aria-describedby="rule-new-keywords-hint">
          <span class="field-hint" id="rule-new-keywords-hint">${wordsHint(defaults.mode)}</span>
        </div>
        <div class="field">
          <span class="field-label" id="rule-new-mode-label">怎么比</span>
          <fieldset class="segmented segmented-fill" aria-labelledby="rule-new-mode-label" aria-describedby="rule-new-mode-explain">
            ${rules.data.modes.map((item) => h`<label class="segment"><input type="radio" name="rule-new-mode" value="${item.code}"${attr(item.code === defaults.mode, "checked")}>${item.label}</label>`)}
          </fieldset>
          <span class="field-hint" id="rule-new-mode-explain">${explain(defaults.mode)}</span>
        </div>
        <div class="field">
          <label class="field-label" for="rule-new-reply">回复</label>
          <textarea class="textarea" id="rule-new-reply" rows="3" spellcheck="false" placeholder="安装包在群文件里～" aria-describedby="rule-new-reply-hint"></textarea>
          <span class="field-hint" id="rule-new-reply-hint">先写一句；图片、更多说法、只在 @ 时回这些，建好后在卡片里加。</span>
        </div>
        <div class="field">
          <label class="field-label" for="rule-new-scope">在哪回</label>
          <span class="select"><select id="rule-new-scope">
            <option value="">所有群（共用）</option>
            ${KINDS.group.roster.list.map((entry) => h`<option value="${entry.id}">只在 ${entry.name || `群 ${entry.id}`}</option>`)}
          </select>${icon("keyboard_arrow_down")}</span>
        </div>
        <div class="actions actions-end">
          <button class="btn btn-text" type="button" value="cancel">取消</button>
          <button class="btn btn-filled" type="submit" value="confirm">添加</button>
        </div>
      </form>`);
    const words = $("#rule-new-keywords", dialog);
    const text = $("#rule-new-reply", dialog);
    let created = null;
    const click = (event) => {
      if (event.target.closest('button[value="cancel"]')) dialog.close("cancel");
    };
    const pickedMode = () => $('input[name="rule-new-mode"]:checked', dialog)?.value || defaults.mode;
    const showMode = (code) => {
      $("#rule-new-mode-explain", dialog).textContent = explain(code);
      $("#rule-new-keywords-hint", dialog).textContent = wordsHint(code);
      words.classList.toggle("mono", code === "regex");
    };
    const change = (event) => {
      if (event.target.name === "rule-new-mode") showMode(event.target.value);
    };
    const typing = (event) => {
      if (event.target.getAttribute("aria-invalid") === "true") clearFieldError(event.target);
      // 写成 /…/ 的就是正则：比法跟着切过去，省得再点一下。
      if (event.target === words && /^\/.+\/$/.test(words.value.trim()) && pickedMode() !== "regex") {
        const radio = $('input[name="rule-new-mode"][value="regex"]', dialog);
        if (radio) {
          radio.checked = true;
          showMode("regex");
        }
      }
    };
    const submit = async (event) => {
      event.preventDefault();
      event.stopPropagation();
      // 正则里的 | 是「或」，不能拆；其余用 | 隔开几个触发词。
      const mode = pickedMode();
      const keywords = words.value.split(mode === "regex" ? /\n/ : /[|｜\n]/).map((word) => word.trim()).filter(Boolean);
      if (!keywords.length) return fieldError(words, "先写一个触发词。"), words.focus();
      if (!text.value.trim()) return fieldError(text, "先写一句回复。"), text.focus();
      const confirm = $('button[value="confirm"]', dialog);
      confirm.setAttribute("aria-busy", "true");
      try {
        const scope = $("#rule-new-scope", dialog).value;
        const body = {
          id: 0,
          keywords,
          mode,
          replies: [{ text: text.value }],
          groups: scope ? [scope] : [],
        };
        created = await api("/autoreply/rules", { method: "POST", body });
        dialog.close("confirm");
      } catch (error) {
        if (error.status === 401) return dialog.close("cancel");
        const target = /回复/.test(error.message) && !/触发词/.test(error.message) ? text : words;
        fieldError(target, error.message);
        target.focus();
      } finally {
        confirm.removeAttribute("aria-busy");
      }
    };
    dialog.addEventListener("click", click);
    dialog.addEventListener("change", change);
    dialog.addEventListener("input", typing);
    dialog.addEventListener("submit", submit);
    dialog.addEventListener(
      "close",
      async () => {
        dialog.removeEventListener("click", click);
        dialog.removeEventListener("change", change);
        dialog.removeEventListener("input", typing);
        dialog.removeEventListener("submit", submit);
        if (!created) {
          if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
          return;
        }
        rules.open.add(created.rule.id);
        await rulesMount({ focus: `[data-rule="${created.rule.id}"] > summary` });
        $(`[data-rule="${created.rule.id}"]`)?.scrollIntoView({ block: "nearest" });
        snackbar(`${created.message}。展开的卡片里还能加图、加说法、限定群。`);
      },
      { once: true }
    );
    dialog.returnValue = "";
    dialog.showModal();
    words.focus();
  }

  /** 回复里插一张图：先收进本机图库，再把 {image:文件名} 插在光标处。 */
  async function tplImage(input) {
    const root = input.closest("[data-tpl]");
    const file = input.files?.[0];
    input.value = "";
    const ctx = tplCtx(root);
    const plugin = ctx && tplSpec(ctx, root.dataset.tpl)?.images;
    if (!file || !plugin) return;
    const button = $("[data-tpl-image]", root);
    const label = $("span", button);
    button.setAttribute("aria-busy", "true");
    label.textContent = "正在上传…";
    try {
      const name = await uploadPhoto(plugin, file);
      const mark = `{image:${name}}`;
      const inputs = $$("[data-tpl-input]", root);
      if (!inputs.length) {
        void tplSave(tplRedraw(root, [mark], 0));
        return;
      }
      const target = inputs[Number(root.dataset.tplActive) || 0] || inputs[0];
      const start = target.selectionStart ?? target.value.length;
      const end = target.selectionEnd ?? start;
      target.focus({ preventScroll: true });
      target.setRangeText(mark, start, end, "end");
      tplRepaint(target);
      tplGrow(target);
      await tplSave(root);
    } catch (error) {
      report(error);
    } finally {
      button.removeAttribute("aria-busy");
      label.textContent = "插入图片";
    }
  }

  /* ==================== §8c 潜水清理的名单面板 ====================
     选一个群、查一遍，名单与群里那张长图同一份（同样的序号）：按类别筛、按名字或号码找、
     勾人或按序号区间选，再「提醒」或「移出」。移出前的确认框写清人数、谁、要多久；
     移出走与群里同一条慢队列，这里显示进度，可以随时停。拿不准的人（待观察）没有勾选框。
     面板的事件挂在自己的根节点上，重画时跟着换。 */

  const lurk = { data: null, group: "", scan: null, filter: "all", query: "", chosen: new Set(), job: null, poll: 0, mounting: 0 };

  const LK_KINDS = [
    { code: "all", label: "全部" },
    { code: "silent", label: "很久没说话" },
    { code: "never", label: "进群后没说过话" },
    { code: "quiet", label: "说得太少" },
    { code: "unsure", label: "待观察" },
  ];

  const lkDate = (at) => {
    if (!at) return "";
    const day = new Date(at * 1000);
    const sameYear = day.getFullYear() === new Date().getFullYear();
    return sameYear ? `${day.getMonth() + 1} 月 ${day.getDate()} 日` : `${day.getFullYear()} 年 ${day.getMonth() + 1} 月 ${day.getDate()} 日`;
  };
  const lkDays = (scan, at) => Math.max(0, Math.floor((scan.at - at) / 86400));
  const lkMinutes = (seconds) => (seconds > 0 ? `${Math.max(1, Math.ceil(seconds / 60))} 分钟` : "不到 1 分钟");
  const lkAvatar = (id) => (/^\d{5,}$/.test(id) ? `https://q1.qlogo.cn/g?b=qq&nk=${id}&s=100` : "");
  const lkInitial = (name) => [...String(name)].find((c) => /[\p{L}\p{N}]/u.test(c))?.toUpperCase() || "?";
  const lkGroupName = (id) => entryName("group", id) || lurk.data?.lists.find((list) => list.group === id)?.group_name || `群 ${id}`;

  function lurkerSection(sub) {
    clearTimeout(lurk.mounting);
    lurk.mounting = setTimeout(() => void lurkerMount(), 0);
    return h`<section class="section lk" id="lurker-panel" aria-labelledby="lurker-title" aria-busy="true">
      <div class="section-head">
        ${raw(`<${sub} class="section-title" id="lurker-title">潜水名单</${sub}>`)}
        <span class="section-meta">与群里「潜水」那张图同一份、同样的序号</span>
      </div>
      <div id="lurker-body"><div class="progress" role="progressbar" aria-label="正在读取"></div></div>
    </section>`;
  }

  async function lurkerMount() {
    const panel = $("#lurker-panel");
    if (!panel) return;
    if (!panel.dataset.bound) {
      panel.dataset.bound = "";
      panel.addEventListener("click", lurkerClick);
      panel.addEventListener("change", lurkerChange);
      panel.addEventListener("input", lurkerInput);
      panel.addEventListener("keydown", (event) => {
        if (event.key === "Enter" && event.target.id === "lk-range") {
          event.preventDefault();
          lurkerRange();
        } else if (event.key === "Enter" && event.target.id === "lk-days") {
          event.preventDefault();
          void lurkerScan(true);
        }
      });
    }
    try {
      lurk.data = await api("/lurker");
      if (!panel.isConnected) return;
      const active = lurk.data.pending[0]?.group;
      lurk.group ||= active || lurk.data.lists[0]?.group || "";
      lurkerPaint();
      void KINDS.group.roster.load().then(() => {
        const select = $("#lk-group");
        if (select) put(select, lurkerGroupChoices());
      });
      const recent = lurk.data.lists.find((list) => list.group === lurk.group);
      if (recent && Date.now() / 1000 - recent.at < 600) void lurkerScan(false);
      else if (lurk.scan?.group === lurk.group) lurkerPaintScan();
      if (lurk.group) void lurkerJob();
    } catch (error) {
      if (error.status === 401) return;
      put($("#lurker-body"), emptyState(`没读出来：${error.message}`, "error"));
    } finally {
      panel.removeAttribute("aria-busy");
    }
  }

  function lurkerGroupChoices() {
    const known = KINDS.group.roster.list.map((entry) => entry.id);
    const scanned = (lurk.data?.lists || []).map((list) => list.group);
    const ids = [...new Set([...scanned, ...known])];
    const option = (id) => h`<option value="${id}"${attr(id === lurk.group, "selected")}>${lkGroupName(id)}${scanned.includes(id) ? " · 查过" : ""}</option>`;
    return [h`<option value=""${attr(!lurk.group, "selected")} disabled>选一个群</option>`, ...ids.map(option)];
  }

  function lurkerPaint() {
    const data = lurk.data;
    put($("#lurker-body"), h`
      ${data.enabled ? "" : h`<div class="rules-off" role="note">${icon("warning-fill")}<span class="rules-off-text"><strong>潜水清理还关着。</strong>能查名单，但移不出人；打开上面的开关再来。</span></div>`}
      ${data.connected ? "" : h`<div class="rules-off" role="note">${icon("info-fill")}<span class="rules-off-text">机器人还没连上账号，连上后才能查名单。</span></div>`}
      <div class="card lk-pick">
        <div class="lk-pick-row">
          <span class="select lk-group"><select id="lk-group" aria-label="查哪个群">${lurkerGroupChoices()}</select>${icon("keyboard_arrow_down")}</span>
          <span class="input-unit lk-days">
            <input class="input num" id="lk-days" inputmode="numeric" autocomplete="off" spellcheck="false" placeholder="${data.rule.days}"
              aria-label="多少天没说话算潜水（留空照配置）" aria-describedby="lk-pick-hint">
            <span class="unit-text">天</span>
          </span>
          <button class="btn btn-filled" type="button" data-lk-scan>${icon("search")}查一遍</button>
        </div>
        <p class="field-hint" id="lk-pick-hint">天数留空照配置（${data.rule.days} 天，单独设过的群照它自己的）；填了只这一次按它看。群主、管理员、号主与豁免名单里的人不会进名单。</p>
      </div>
      <div id="lk-job" aria-live="polite"></div>
      <div id="lk-result"></div>
      ${data.history.length ? lurkerHistory(data.history) : ""}`);
  }

  async function lurkerScan(again) {
    const group = $("#lk-group")?.value || lurk.group;
    if (!group) return snackbar("先选一个群", "error");
    const daysText = $("#lk-days")?.value.trim() || "";
    const days = daysText ? Number(daysText) : null;
    if (daysText && !(Number.isInteger(days) && days >= 1 && days <= 3650)) return snackbar("天数写 1 到 3650 之间的整数", "error");
    lurk.group = group;
    const button = $("[data-lk-scan]");
    button?.setAttribute("aria-busy", "true");
    put($("#lk-result"), h`<div class="card lk-loading"><div class="progress" role="progressbar" aria-label="正在查"></div><p class="note">正在拉全群名单、对消息记录，大群要几秒钟…</p></div>`);
    try {
      lurk.scan = await api("/lurker/scan", { method: "POST", body: { group, days, again } });
      lurk.chosen.clear();
      lurk.filter = "all";
      lurk.query = "";
      lurkerPaintScan();
      void lurkerJob();
    } catch (error) {
      put($("#lk-result"), h`<div class="card">${emptyState(`没查成：${error.message}`, "error")}</div>`);
      report(error);
    } finally {
      button?.removeAttribute("aria-busy");
    }
  }

  const lkAll = () => [...(lurk.scan?.entries || []), ...(lurk.scan?.unsure || [])];
  const lkVisible = () => {
    const query = lurk.query.normalize("NFKC").toLowerCase();
    return lkAll().filter((entry) => (lurk.filter === "all" || entry.kind === lurk.filter) && (!query || `${entry.no} ${entry.name} ${entry.nick} ${entry.id} ${entry.title || ""}`.normalize("NFKC").toLowerCase().includes(query)));
  };

  function lurkerPaintScan() {
    const scan = lurk.scan;
    if (!scan) return;
    const count = (kind) => lkAll().filter((entry) => entry.kind === kind).length;
    const kept = scan.protected.map(([label, n]) => `${label} ${n}`);
    if (scan.newcomers) kept.push(`进群不满 ${scan.rule.days} 天的新人 ${scan.newcomers}`);
    put($("#lk-result"), h`
      <div class="card lk-head">
        <p class="lk-summary">「${scan.group_name}」全群 <b>${num(scan.total)}</b> 人，按「${scan.rule.text}」查到 <b>${num(scan.entries.length)}</b> 人${scan.entries.length ? "" : "，没有要清理的"}。
          <span class="lk-stamp">${ago(scan.at)}查的</span>
          <button class="btn btn-text" type="button" data-lk-rescan>${icon("refresh")}重新查</button></p>
        ${scan.note ? h`<p class="lk-note" role="note">${icon("info-fill")}<span>${scan.note}</span></p>` : ""}
        ${scan.can_kick ? "" : h`<p class="lk-note" role="note">${icon("warning-fill")}<span>机器人在这个群里不是管理员，只能看，移不出人。</span></p>`}
        ${kept.length ? h`<p class="field-hint">不在名单上的：${kept.join(" · ")}</p>` : ""}
      </div>
      ${lkAll().length
        ? h`<div class="chips chips-scroll lk-kinds" role="group" aria-label="按类别筛选">${LK_KINDS.filter((kind) => kind.code === "all" || count(kind.code)).map((kind) => h`<button class="chip" type="button" data-lk-kind="${kind.code}" aria-pressed="${bool(lurk.filter === kind.code)}">${icon("check")}${kind.label}<span class="chip-count">${kind.code === "all" ? lkAll().length : count(kind.code)}</span></button>`)}</div>
          <div class="lk-tools">
            <div class="search">${icon("search")}<input class="input" id="lk-search" type="search" placeholder="按名字、号码、头衔或序号找" aria-label="在名单里找" autocomplete="off" spellcheck="false" value="${lurk.query}"></div>
            ${scan.entries.length
              ? h`<div class="lk-range">
                  <input class="input" id="lk-range" type="text" placeholder="按序号选：1-20 25" aria-label="按序号选" aria-describedby="lk-range-hint" autocomplete="off" spellcheck="false" enterkeyhint="done">
                  <button class="btn btn-tonal" type="button" data-lk-range>选中</button>
                </div>`
              : ""}
          </div>
          <p class="field-hint lk-range-hint" id="lk-range-hint">序号可以写区间与几段：「1-20 25」「全部 除 3 7」「1-30 !5」。</p>
          <p class="field-hint lk-warn-line" id="lk-warn-line">${lurkerWarnLine()}</p>
          <p class="section-meta" id="lk-status" role="status"></p>
          <div class="lk-scroll" tabindex="0" role="region" aria-label="潜水名单，可滚动"><ol class="lk-list" id="lk-list"></ol></div>
          <div class="lk-bar" id="lk-bar"></div>`
        : ""}`);
    lurkerPaintRows();
  }

  function lurkerRow(entry) {
    const scan = lurk.scan;
    const days = lkDays(scan, entry.since);
    const gone = entry.kind === "quiet" ? `${entry.said} 句` : entry.kind === "unsure" ? `${days}+ 天` : entry.last || entry.kind === "never" ? `${days} 天` : `≥${days} 天`;
    const last = entry.kind === "never" ? "进群后没说过话" : entry.kind === "unsure" ? (entry.last ? `记录不全 · 见过 ${lkDate(entry.last)}` : "记录不全 · 没见过他说话") : entry.last ? `最后 ${lkDate(entry.last)}` : "没有发言记录";
    const meta = [entry.nick && entry.nick !== entry.name ? entry.nick : "", entry.id, entry.level ? `LV${entry.level}` : "", entry.title ? `「${entry.title}」` : "", entry.joined ? `${lkDate(entry.joined)}进群` : ""].filter(Boolean).join(" · ");
    const src = lkAvatar(entry.id);
    const face = src
      ? h`<span class="lk-face" aria-hidden="true"><img src="${src}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer" data-initial="${lkInitial(entry.name)}"></span>`
      : h`<span class="lk-face" aria-hidden="true">${lkInitial(entry.name)}</span>`;
    const checked = lurk.chosen.has(entry.id);
    const sure = entry.no > 0 && scan.can_kick;
    const removed = lurk.job?.done?.find((outcome) => outcome.id === entry.id);
    const tags = h`${entry.warned ? h`<span class="status">${ago(entry.warned)}提醒过</span>` : ""}${removed ? h`<span class="status ${removed.result === "removed" ? "status-success" : ""}">${{ removed: "已移出", spoke: "冒泡了，留下", left: "已不在群里", failed: "没移出去", skipped: "没轮到" }[removed.result]}</span>` : ""}`;
    const body = h`<span class="lk-no">${entry.no || "—"}</span>${face}
      <span class="lk-who"><span class="lk-name">${entry.name}${tags}</span><span class="lk-meta">${meta}</span></span>
      <span class="lk-when"><span class="lk-gone">${gone}</span><span class="lk-last">${last}</span></span>`;
    return h`<li class="lk-row${checked ? " lk-on" : ""}${removed?.result === "removed" ? " lk-gone-row" : ""}" data-lk-id="${entry.id}"${attr(entry.kind === "unsure", "data-unsure")}>
      ${sure && !removed
        ? h`<label class="lk-hit"><input type="checkbox" class="lk-check" value="${entry.id}"${attr(checked, "checked")} aria-label="选中 ${entry.no} 号 ${entry.name}（${entry.id}）">${body}</label>`
        : h`<div class="lk-hit"><span class="lk-check-none" aria-hidden="true"></span>${body}</div>`}
      <button class="icon-btn state lk-exempt" type="button" data-lk-exempt="${entry.id}" aria-label="豁免 ${entry.name}：以后不进这个群的名单" title="豁免：以后不进这个群的名单">${icon("shield_person")}</button>
    </li>`;
  }

  /** 名单很长时只先画前 200 行，往下有「再显示」——几百行一次画完，手机上会卡一下。
   *  名单在一个限高的滚动区里：再长也不把下面的配置挤到看不见的地方，操作条始终贴在名单下面。 */
  function lurkerPaintRows(limit = 200) {
    const list = $("#lk-list");
    if (!list) return;
    const rows = lkVisible();
    put(list, h`${rows.slice(0, limit).map(lurkerRow)}${rows.length > limit ? h`<li class="lk-more"><button class="btn btn-tonal" type="button" data-lk-more="${limit + 200}">再显示 ${Math.min(200, rows.length - limit)} 人（共 ${rows.length}）</button></li>` : ""}`);
    if (!rows.length) put(list, h`<li>${emptyState(lurk.query ? "没有对得上的人。" : "这一类没有人。", "search")}</li>`);
    const status = $("#lk-status");
    if (status) status.textContent = lurk.filter === "all" && !lurk.query ? "" : `找到 ${rows.length} 人`;
    lurkerPaintBar();
  }

  function lurkerPaintBar() {
    const bar = $("#lk-bar");
    if (!bar || !lurk.scan) return;
    const n = lurk.chosen.size;
    const total = lurk.scan.entries.length;
    const sure = total && lurk.scan.can_kick;
    const busy = lurk.job && ["running", "interrupted"].includes(lurk.job.state);
    const pending = lurk.data?.warn_pending?.[lurk.scan.group] || 0;
    put(bar, sure
      ? h`<span class="lk-count" role="status">${n ? h`已选 <b>${n}</b> 人` : "勾选要处理的人，或按序号选"}${pending ? h`<span class="lk-pending">${pending} 人等着到期移出</span>` : ""}</span>
          <div class="lk-actions">
            ${n ? h`<button class="btn btn-text" type="button" data-lk-clear>清空</button>` : h`<button class="btn btn-text" type="button" data-lk-all>选这一类</button>`}
            <button class="btn btn-outlined" type="button" data-lk-warn-all>${icon("campaign")}提醒全部</button>
            <button class="btn btn-tonal" type="button" data-lk-warn${attr(!n, "disabled")}>${icon("notifications_active")}提醒${n ? ` ${n} 人` : ""}</button>
            <button class="btn btn-filled btn-danger" type="button" data-lk-kick${attr(!n || busy, "disabled")}>${icon("person_remove")}${n ? `移出 ${n} 人` : "移出"}</button>
          </div>`
      : "");
    bar.hidden = !sure;
  }

  function lurkerChosenEntries() {
    return lurk.scan.entries.filter((entry) => lurk.chosen.has(entry.id));
  }

  /** 「1-20 25」「全部 除 3 7」「1-30 !5」→ 序号。与群里「潜水 踢」同一套写法。 */
  function lkPicks(text, max) {
    const normal = text.normalize("NFKC").replace(/[~～—–－到至]/g, "-").replace(/[，,、；;]/g, " ");
    let chosen = [];
    const dropped = new Set();
    let excluding = false;
    for (let token of normal.split(/\s+/).filter(Boolean)) {
      let negate = false;
      if (token.startsWith("!")) {
        negate = true;
        token = token.slice(1);
      }
      if (["除", "除了", "不要", "排除", "不含"].includes(token)) {
        excluding = true;
        continue;
      }
      let range;
      if (["全部", "所有", "全", "all"].includes(token)) range = Array.from({ length: max }, (_, i) => i + 1);
      else {
        const [a, b = a] = token.split("-").map(Number);
        if (!Number.isInteger(a) || !Number.isInteger(b) || a < 1 || b < 1 || a > max || b > max) throw new Error(`「${token}」不在名单上（1-${max}）`);
        range = Array.from({ length: Math.abs(b - a) + 1 }, (_, i) => Math.min(a, b) + i);
      }
      for (const n of range) (excluding || negate ? dropped.add(n) : chosen.push(n));
    }
    if (!chosen.length && dropped.size) chosen = Array.from({ length: max }, (_, i) => i + 1);
    return [...new Set(chosen)].filter((n) => !dropped.has(n));
  }

  function lurkerRange() {
    const input = $("#lk-range");
    if (!input || !lurk.scan) return;
    try {
      const picks = lkPicks(input.value, lurk.scan.entries.length);
      if (!picks.length) return snackbar("写几个序号，比如 1-20 25", "error");
      const ids = lurk.scan.entries.filter((entry) => picks.includes(entry.no)).map((entry) => entry.id);
      lurk.chosen = new Set(ids);
      lurkerPaintRows();
      snackbar(`选中了 ${ids.length} 人`);
    } catch (error) {
      snackbar(error.message, "error");
    }
  }

  async function lurkerKick() {
    const scan = lurk.scan;
    const chosen = lurkerChosenEntries();
    if (!chosen.length) return;
    const pace = lurk.data.pace;
    if (chosen.length > pace.max_per_run) return snackbar(`一次最多移出 ${pace.max_per_run} 人；分几次来，或调高下面的「一次最多移出」`, "error");
    const rests = pace.burst ? Math.floor((chosen.length - 1) / pace.burst) : 0;
    const eta = chosen.length * pace.interval_seconds + rests * pace.rest_seconds;
    const half = chosen.length * 2 > scan.total ? `这是全群 ${scan.total} 人里的一半以上。` : "";
    const ok = await confirmAction({
      title: `从「${scan.group_name}」移出 ${chosen.length} 人？`,
      body: `${half}分批慢慢移出，大约 ${lkMinutes(eta)}；排队时说了话的会留下，可以随时停。${lurk.data.permanent ? "移出后他们不能再申请加群。" : ""}`,
      detail: h`<ol class="lk-confirm">${chosen.map((entry) => h`<li><b>${entry.no}</b> ${entry.name}（${entry.id}）</li>`)}</ol>`,
      confirm: `移出 ${chosen.length} 人`,
      danger: true,
    });
    if (!ok) return;
    try {
      const status = await api("/lurker/kick", { method: "POST", body: { group: scan.group, at: scan.at, ids: chosen.map((entry) => entry.id) } });
      lurk.chosen.clear();
      lurkerJobPaint(status);
      lurkerPaintRows();
      snackbar(`开始移出 ${chosen.length} 人`);
    } catch (error) {
      report(error);
    }
  }

  /** 操作条上方那句：当前的提醒方式与到期策略，省得去配置里翻。 */
  function lurkerWarnLine() {
    const warn = lurk.data?.warn;
    if (!warn) return "";
    const after = warn.escalate
      ? `${warn.grace_days} 天后还没冒泡的会自动移出`
      : `提醒只打招呼，不会自动移出`;
    return `提醒方式：${warn.mode_name || "群里 @ 每个人"}；${after}。在下面的「提醒」一节里改。`;
  }

  /** 提醒的描述：按配置里的方式说清会发生什么，别让人按下去才知道。 */
  function lurkerWarnWords() {
    const warn = lurk.data?.warn || {};
    const mode = warn.mode_name || "群里 @ 每个人";
    const will = {
      mention: `会在群里 @ 他们（一条最多 ${warn.batch || 20} 人）`,
      digest: "会在群里发一条，把名字列出来、不 @ 任何人",
      private: "会挨个私聊他们，群里一句话不说",
      mention_private: `会在群里 @ 他们（一条最多 ${warn.batch || 20} 人），再挨个私聊`,
      digest_private: "会在群里发一条列出名字，再挨个私聊",
    }[warn.mode] || "会在群里 @ 他们";
    const after = warn.escalate
      ? `${warn.grace_days || 3} 天后还没冒泡的会自动移出。`
      : "提醒只打招呼，不会自动移出；到时候重新查一遍名单再说。";
    return { mode, body: `${will}，说清 ${warn.grace_days || 3} 天没说话的宽限。${after}提醒的话在下面的配置里改。` };
  }

  async function lurkerWarn(all) {
    const scan = lurk.scan;
    const chosen = all ? scan.entries : lurkerChosenEntries();
    if (!chosen.length) return;
    const words = lurkerWarnWords(chosen.length);
    const ok = await confirmAction({
      title: all ? `提醒「${scan.group_name}」名单上全部 ${chosen.length} 人？` : `在「${scan.group_name}」提醒 ${chosen.length} 人？`,
      body: words.body,
      confirm: `提醒 ${chosen.length} 人`,
    });
    if (!ok) return;
    try {
      const reply = await api("/lurker/warn", { method: "POST", body: { group: scan.group, at: scan.at, ids: all ? [] : chosen.map((entry) => entry.id) } });
      const now = Date.now() / 1000;
      for (const entry of chosen) entry.warned = now;
      if (!all) lurk.chosen.clear();
      lurkerPaintRows();
      snackbar(reply.message);
      void api("/lurker").then((data) => {
        lurk.data = data;
        lurkerPaintBar();
      });
    } catch (error) {
      report(error);
    }
  }

  async function lurkerExempt(id) {
    const scan = lurk.scan;
    const entry = lkAll().find((item) => item.id === id);
    if (!scan || !entry) return;
    try {
      await api("/lurker/exempt", { method: "POST", body: { group: scan.group, id, on: true } });
      const restore = { entries: [...scan.entries], unsure: [...scan.unsure] };
      scan.entries = scan.entries.filter((item) => item.id !== id);
      scan.unsure = scan.unsure.filter((item) => item.id !== id);
      lurk.chosen.delete(id);
      lurkerPaintRows();
      snackbar(`${entry.name} 以后不进这个群的名单`, "info", {
        label: "撤销",
        run: async () => {
          try {
            await api("/lurker/exempt", { method: "POST", body: { group: scan.group, id, on: false } });
            Object.assign(scan, restore);
            lurkerPaintRows();
          } catch (error) {
            report(error);
          }
        },
      });
    } catch (error) {
      report(error);
    }
  }

  async function lurkerJob() {
    if (!lurk.group) return;
    try {
      lurkerJobPaint(await api(`/lurker/job?group=${encodeURIComponent(lurk.group)}`));
    } catch (error) {
      if (error.status !== 401) put($("#lk-job"), "");
    }
  }

  function lurkerJobPaint(status) {
    const slot = $("#lk-job");
    clearTimeout(lurk.poll);
    if (!slot) return;
    const job = status.active;
    const wasRunning = lurk.job && lurk.job.state === "running";
    lurk.job = job || (lurk.scan && status.last && status.last.created >= lurk.scan.at ? status.last : null);
    if (!job) {
      put(slot, "");
      if (wasRunning && status.last) {
        snackbar(status.last.summary.split("\n")[0]);
        lurkerPaintRows();
        void api("/lurker").then((data) => {
          lurk.data = data;
          const history = $("#lk-history");
          if (history) history.outerHTML = part(lurkerHistory(data.history));
        });
      }
      return;
    }
    const handled = job.total - job.left;
    const interrupted = job.state === "interrupted";
    put(slot, h`<div class="card lk-job" data-state="${job.state}">
      <div class="lk-job-head">
        <span class="lk-job-title">${interrupted ? "上次的移出被打断了" : `正在从「${job.group_name}」移出`}</span>
        <button class="btn btn-outlined" type="button" data-lk-stop>${icon("stop_circle")}${interrupted ? "作罢" : "停下"}</button>
      </div>
      <meter class="meter" min="0" max="${job.total}" value="${handled}" aria-label="移出进度">${handled} / ${job.total}</meter>
      <p class="lk-job-line">已处理 <b>${handled}</b> / ${job.total} 人：移出 ${job.removed}${job.spoke ? ` · 冒泡留下 ${job.spoke}` : ""}${job.gone ? ` · 已不在群里 ${job.gone}` : ""}${job.failed ? ` · 没移出去 ${job.failed}` : ""}${interrupted ? `。还剩 ${job.left} 人，在群里发「潜水 继续」接着做。` : `。还剩 ${job.left} 人，大约 ${lkMinutes(job.eta)}。`}</p>
    </div>`);
    if (!interrupted) lurk.poll = setTimeout(() => $("#lk-job") && void lurkerJob(), 3000);
    lurkerPaintRows();
  }

  function lurkerHistory(history) {
    const when = (at) => new Date(at * 1000).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
    const who = { group: (job) => job.operator_name || job.operator, console: () => "控制台", auto: () => "自动清理" };
    return h`<details class="memory lk-history" id="lk-history">
      <summary class="state">
        <span class="item-leading" aria-hidden="true">${icon("history")}</span>
        <span class="item-content"><span class="item-title">最近的移出</span><span class="item-sub">${history.length} 次 · 谁确认的、移出了谁</span></span>
        ${icon("chevron_right", "chevron")}
      </summary>
      <ul class="memory-body lk-history-list">${history.map((job) => {
        const removed = job.done.filter((outcome) => outcome.result === "removed");
        return h`<li class="lk-history-row">
          <span class="item-title">${when(job.created)} · ${job.group_name} · ${(who[job.source] || who.console)(job)}</span>
          <span class="item-sub">${job.summary}</span>
          ${removed.length ? h`<span class="lk-history-names">${removed.map((outcome) => `${outcome.name}（${outcome.id}）`).join("、")}</span>` : ""}
        </li>`;
      })}</ul>
    </details>`;
  }

  function lurkerClick(event) {
    const on = (selector) => event.target.closest(selector);
    let hit;
    if (on("[data-lk-scan]")) return void lurkerScan(false);
    if (on("[data-lk-rescan]")) return void lurkerScan(true);
    if ((hit = on("[data-lk-kind]"))) {
      lurk.filter = hit.dataset.lkKind;
      for (const chip of $$("[data-lk-kind]")) chip.setAttribute("aria-pressed", bool(chip === hit));
      return lurkerPaintRows();
    }
    if ((hit = on("[data-lk-more]"))) return lurkerPaintRows(Number(hit.dataset.lkMore));
    if (on("[data-lk-range]")) return lurkerRange();
    if (on("[data-lk-clear]")) {
      lurk.chosen.clear();
      return lurkerPaintRows();
    }
    if (on("[data-lk-all]")) {
      for (const entry of lkVisible()) if (entry.no > 0) lurk.chosen.add(entry.id);
      return lurkerPaintRows();
    }
    if (on("[data-lk-kick]")) return void lurkerKick();
    if (on("[data-lk-warn-all]")) return void lurkerWarn(true);
    if (on("[data-lk-warn]")) return void lurkerWarn(false);
    if ((hit = on("[data-lk-exempt]"))) return void lurkerExempt(hit.dataset.lkExempt);
    if ((hit = on("[data-lk-stop]"))) {
      hit.setAttribute("aria-busy", "true");
      return void api("/lurker/stop", { method: "POST", body: { group: lurk.group } })
        .then((reply) => snackbar(reply.message))
        .catch(report)
        .finally(() => void lurkerJob());
    }
  }

  function lurkerChange(event) {
    const target = event.target;
    if (target.id === "lk-group") {
      lurk.group = target.value;
      lurk.scan = null;
      lurk.job = null;
      lurk.chosen.clear();
      put($("#lk-result"), "");
      const recent = lurk.data?.lists.find((list) => list.group === lurk.group);
      if (recent && Date.now() / 1000 - recent.at < 600) void lurkerScan(false);
      void lurkerJob();
      return;
    }
    if (target.matches(".lk-check")) {
      if (target.checked) lurk.chosen.add(target.value);
      else lurk.chosen.delete(target.value);
      target.closest(".lk-row")?.classList.toggle("lk-on", target.checked);
      lurkerPaintBar();
    }
  }

  function lurkerInput(event) {
    if (event.target.id === "lk-search") {
      lurk.query = event.target.value.trim();
      lurkerPaintRows();
    }
  }

  /* ==================== §9 搭话 ==================== */

  const SOURCES = [
    { name: "persona", file: "persona.md", title: "人格", note: "它在群里说话的方式与分寸。" },
    { name: "self", file: "self.md", title: "档案", note: "它对自己是谁的认识。" },
  ];
  const STICKER_PAGE = 60;
  const ambient = { data: null, tab: "persona", drafts: {}, stickers: STICKER_PAGE, mounting: 0 };

  const sourceText = (name) => ambient.drafts[name] ?? ambient.data?.[name] ?? "";
  const dirty = (name) => name in ambient.drafts && ambient.drafts[name] !== (ambient.data?.[name] ?? "");

  function editorState(name) {
    return `${num(count(sourceText(name)))} 字 · ${dirty(name) ? "有未保存的修改" : "已保存"}`;
  }

  /** 搭话资料是 ambient 插件详情里的一节：人格、记忆与表情包都在这里改，
   *  不另开一页，也不占一个底部导航入口。先铺骨架，再异步把数据填进去。 */
  function ambientSection(sub) {
    clearTimeout(ambient.mounting);
    ambient.mounting = setTimeout(() => void ambientMount(), 0);
    return h`<section class="section ambient-section" id="ambient-panel" aria-labelledby="ambient-title" aria-busy="true">
      <div class="section-head">
        ${raw(`<${sub} class="section-title" id="ambient-title">搭话资料</${sub}>`)}
        <span class="section-meta">人格、记忆与表情包</span>
      </div>
      <div id="ambient-body">${ambient.data ? ambientBody(ambient.data) : h`<div class="progress" role="progressbar" aria-label="正在读取搭话资料"></div>`}</div>
    </section>`;
  }

  async function ambientMount() {
    const panel = $("#ambient-panel");
    if (!panel) return;
    try {
      const data = await api("/ambient");
      if (!panel.isConnected) return;
      ambient.data = data;
      put($("#ambient-body"), ambientBody(data));
      panel.removeAttribute("aria-busy");
      paintGroupNames();
    } catch (error) {
      if (!panel.isConnected) return;
      put($("#ambient-body"), h`<div class="card">${emptyState(error.message || "读不到搭话资料。", "error")}</div>`);
      panel.removeAttribute("aria-busy");
    }
  }

  function ambientBody(data) {
    if (!data.ready) {
      return h`<div class="card">${emptyState("搭话插件的数据目录还没有建立。让它运行一轮之后再来。")}</div>`;
    }
    const tabs = [
      { id: "persona", label: "人设" },
      { id: "memory", label: "记忆", count: data.memory.length },
      { id: "stickers", label: "表情包", count: data.stickers.length },
    ];
    return h`<div>
      <p class="note ambient-summary">记得 ${data.memory.length} 个群里的人与事，收藏了 ${data.stickers.length} 张表情包。保存后下一轮对话生效。</p>
      <div class="tabs" role="tablist" aria-label="搭话资料">
        ${tabs.map(
          (tab) => h`<button class="tab state" type="button" role="tab" id="tab-${tab.id}" data-tab="${tab.id}"
            aria-controls="panel-${tab.id}" aria-selected="${bool(ambient.tab === tab.id)}" tabindex="${ambient.tab === tab.id ? 0 : -1}">
            ${tab.label}${tab.count !== undefined ? h`<span class="tab-count">${tab.count}</span>` : ""}</button>`
        )}
      </div>
      <div class="tabpanel" role="tabpanel" id="panel-persona" aria-labelledby="tab-persona"${attr(ambient.tab !== "persona", "hidden")}>
        ${SOURCES.map(sourceEditor)}
      </div>
      <div class="tabpanel" role="tabpanel" id="panel-memory" aria-labelledby="tab-memory" tabindex="0"${attr(ambient.tab !== "memory", "hidden")}>
        ${memoryPanel(data.memory)}
      </div>
      <div class="tabpanel" role="tabpanel" id="panel-stickers" aria-labelledby="tab-stickers" tabindex="0"${attr(ambient.tab !== "stickers", "hidden")}>
        ${stickerPanel()}
      </div>
    </div>`;
  }

  function sourceEditor(source) {
    return h`<section class="card editor" aria-labelledby="source-${source.name}-title">
      <div class="editor-head">
        <h2 class="editor-title" id="source-${source.name}-title">${source.title}</h2>
        <code class="editor-file">${source.file}</code>
      </div>
      <p class="note" id="source-${source.name}-note">${source.note}保存后下一轮对话生效，旧版本会备份在同一目录。</p>
      <textarea class="textarea editor-text" id="source-${source.name}" data-source="${source.name}" spellcheck="false"
        aria-labelledby="source-${source.name}-title" aria-describedby="source-${source.name}-note">${sourceText(source.name)}</textarea>
      <div class="editor-foot">
        <span class="editor-state" id="source-${source.name}-state"${attr(dirty(source.name), "data-dirty")}>${editorState(source.name)}</span>
        <button class="btn btn-filled" type="button" data-save-source="${source.name}" aria-label="保存${source.title}"${attr(!dirty(source.name), "disabled")}>保存</button>
      </div>
    </section>`;
  }

  function memoryPanel(groups) {
    if (!groups.length) return h`<div class="card">${emptyState("它还没有记住任何群。")}</div>`;
    return h`<div class="group">${groups.map((group, index) => {
      const noted = group.people.filter((person) => person.note || person.address);
      const others = group.people.length - noted.length;
      return h`<details class="memory"${attr(index === 0, "open")}>
        <summary class="state">
          <span class="item-leading" aria-hidden="true">${icon("groups")}</span>
          <span class="item-content">
            <span class="item-title" data-group-name="${group.group}">群 ${group.group}</span>
            <span class="item-sub"><span data-group-id hidden>群号 ${group.group} · </span>${group.people.length} 人 · ${group.notes.length} 条旧事</span>
          </span>
          ${icon("chevron_right", "chevron")}
        </summary>
        <div class="memory-body">
          <p class="note">${noted.length ? `对 ${noted.length} 位有印象${others > 0 ? `，另有 ${others} 位只记得来过` : ""}。` : "还没有对这个群里的任何人写下印象。"}</p>
          ${noted.length
            ? h`<h3 class="subhead">印象</h3><ul class="group">${noted.map(
                (person) => h`<li class="item">
                  <div class="item-content">
                    <span class="item-title">${person.name || person.id}${person.address ? h`<span class="tag">称呼「${person.address}」</span>` : ""}</span>
                    <span class="item-sub">${person.note || "记得称呼，还没有写下印象"}</span>
                  </div>
                  <span class="item-meta">${num(person.messages)} 条<br>${ago(person.last_seen)}</span>
                </li>`
              )}</ul>`
            : ""}
          ${group.notes.length
            ? h`<h3 class="subhead">旧事</h3><ul class="group">${group.notes.map(
                (note) => h`<li class="item"><div class="item-content"><span class="item-sub">${note.text}</span></div>
                  <span class="item-meta">${ago(note.at)}</span></li>`
              )}</ul>`
            : ""}
        </div>
      </details>`;
    })}</div>`;
  }

  /** 记忆按群号存，人要看的是群名：页面出来后向名册问一次，把标题换成群名、群号退到副标题。
   *  问不到（实现端没连上）就保持「群 123456」，不影响阅读。 */
  function paintGroupNames() {
    void KINDS.group.roster.load().then((roster) => {
      for (const title of $$("[data-group-name]")) {
        const entry = roster.byId.get(title.dataset.groupName);
        if (!entry?.name) continue;
        title.textContent = entry.name;
        const id = title.parentElement.querySelector("[data-group-id]");
        if (id) id.hidden = false;
      }
    });
  }

  function stickerPanel() {
    const all = ambient.data.stickers;
    if (!all.length) return h`<div class="card">${emptyState("还没有收藏表情包。它在群里收下一张就会存一张。")}</div>`;
    const shownList = all.slice(0, ambient.stickers);
    const rest = all.length - shownList.length;
    return h`<ul class="stickers" id="sticker-grid">${shownList.map(
      (sticker) => h`<li><figure class="sticker">
        ${sticker.image
          ? h`<img class="sticker-img" loading="lazy" decoding="async" width="132" height="132" alt="${sticker.label || "未命名表情包"}"
              src="/api/ambient/sticker/${sticker.id}?t=${encodeURIComponent(token)}">`
          : h`<span class="sticker-img">商城表情<br>只保存了参数</span>`}
        <figcaption>
          ${sticker.image ? h`<button class="btn btn-text" type="button" data-sticker-play="${sticker.id}" aria-pressed="false">播放原图</button>` : ""}
          <span class="sticker-label" title="${sticker.label}">${sticker.label || "未命名表情包"}</span>
          <span class="sticker-meta">#${sticker.id} · 用过 ${sticker.uses} 次 · ${ago(sticker.added_at)}</span>
        </figcaption>
      </figure></li>`
    )}</ul>
    ${rest > 0
      ? h`<div class="actions"><button class="btn btn-tonal" type="button" data-more-stickers>再显示 ${Math.min(rest, STICKER_PAGE)} 张（还有 ${rest} 张）</button></div>`
      : ""}`;
  }

  function selectTab(tab, focus = false) {
    ambient.tab = tab.dataset.tab;
    for (const item of $$('[role="tab"]')) {
      const on = item === tab;
      item.setAttribute("aria-selected", String(on));
      item.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(item.getAttribute("aria-controls"));
      if (panel) panel.hidden = !on;
    }
    if (focus) tab.focus();
  }

  function onSourceInput(area) {
    const name = area.dataset.source;
    ambient.drafts[name] = area.value;
    const state = $(`#source-${name}-state`);
    state.textContent = editorState(name);
    state.toggleAttribute("data-dirty", dirty(name));
    const button = $(`[data-save-source="${name}"]`);
    button.disabled = button.getAttribute("aria-busy") === "true" || !dirty(name);
  }

  async function saveSource(button) {
    if (button.getAttribute("aria-busy") === "true") return;
    const name = button.dataset.saveSource;
    const text = sourceText(name);
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = await api("/ambient/source", { method: "POST", body: { name, text } });
      ambient.data[name] = text;
      if (ambient.drafts[name] === text) delete ambient.drafts[name];
      const state = $(`#source-${name}-state`);
      if (state) {
        state.textContent = editorState(name);
        state.toggleAttribute("data-dirty", dirty(name));
      }
      snackbar(reply.message);
    } catch (error) {
      button.disabled = false;
      report(error);
    } finally {
      button.removeAttribute("aria-busy");
      button.disabled = !dirty(name);
    }
  }

  /* ==================== §10 日志 ====================
     跟随是阅读意图：真正向上滚动才暂停，回到底部恢复；尺寸变化只在跟随时贴底。
     暂停时不动 DOM（保住选区与位置），新行只进有界缓冲。
     先筛选、后截断，密集的 INFO 淹不掉最新那条 WARN。
     标签页进后台就断开实时连接，回来由服务端快照补齐。 */

  const DOM_LINES = 100;
  const BATCH_MS = 100;
  const INSERT_MAX = 40;
  const STICK = 24;
  const RETRY_BASE = 2000;
  const RETRY_MAX = 30000;
  const BUFFER = 2000;
  const LEVELS = [
    ["", "全部"],
    ["INFO", "信息"],
    ["WARN", "警告"],
    ["ERRO", "错误"],
    ["DEBG", "调试"],
  ];

  function logRow(entry) {
    const level = String(entry.level || "INFO");
    return h`<div class="log-row" data-level="${level.toLowerCase()}"><time class="log-time">${entry.at}</time><span class="log-level">${level}</span><span class="log-target">${entry.target}</span><span class="log-text">${entry.text}</span></div>`;
  }

  const logs = {
    lines: [],
    level: "",
    query: "",
    follow: true,
    source: null,
    queue: [],
    timer: 0,
    mounted: false,
    status: "正在连接…",
    fresh: 0,
    failures: 0,
    retry: 0,
    resize: null,
    top: 0,

    matches(entry) {
      if (this.level && entry.level !== this.level) return false;
      const text = this.query.trim().toLowerCase();
      return !text || String(entry.text).toLowerCase().includes(text) || String(entry.target).toLowerCase().includes(text);
    },

    box: () => $("#log-box"),
    stuck: (box) => box.scrollHeight - box.scrollTop - box.clientHeight <= STICK,

    pin(box) {
      box.scrollTop = box.scrollHeight;
      this.top = box.scrollTop;
    },

    chrome() {
      const box = this.box();
      const counter = $("#log-count");
      if (counter) {
        const visible = box ? box.childElementCount - ($(".log-empty", box) ? 1 : 0) : 0;
        counter.textContent = `显示 ${visible} 条 · 缓冲 ${this.lines.length} 条`;
      }
      const follow = $("#log-follow");
      if (follow) {
        follow.setAttribute("aria-pressed", String(this.follow));
        follow.classList.toggle("btn-tonal", this.follow);
        follow.classList.toggle("btn-outlined", !this.follow);
      }
      const jump = $("#log-jump");
      if (jump) {
        jump.hidden = this.follow;
        const label = this.fresh ? `回到最新 · ${this.fresh} 条新记录` : "回到最新";
        const span = $("span", jump);
        if (span && span.textContent !== label) span.textContent = label;
      }
    },

    setStatus(text) {
      this.status = text;
      const status = $("#log-status");
      if (status && status.textContent !== text) status.textContent = text;
    },

    paint() {
      const box = this.box();
      if (!box || document.hidden) return;
      clearTimeout(this.timer);
      this.timer = 0;
      this.queue = [];
      const shownLines = this.lines.filter((entry) => this.matches(entry)).slice(-DOM_LINES);
      box.innerHTML = shownLines.length
        ? part(shownLines.map(logRow))
        : part(h`<p class="log-empty">${this.lines.length ? "没有符合条件的日志" : "还没有日志"}</p>`);
      this.fresh = 0;
      this.chrome();
      if (this.follow) this.pin(box);
    },

    setFollow(on) {
      const box = this.box();
      this.top = box?.scrollTop || 0;
      if (this.follow !== on) {
        this.follow = on;
        if (on) this.fresh = 0;
        this.chrome();
      }
      if (on && box) this.paint();
    },

    push(entries) {
      this.lines.push(...entries);
      if (this.lines.length > BUFFER) this.lines.splice(0, this.lines.length - BUFFER);
      const matching = entries.filter((entry) => this.matches(entry));
      if (document.hidden || !this.mounted || !this.follow) {
        this.fresh += matching.length;
        this.chrome();
        return;
      }
      this.queue.push(...matching);
      if (this.queue.length > DOM_LINES) this.queue.splice(0, this.queue.length - DOM_LINES);
      if (!this.timer) this.timer = setTimeout(() => this.flush(), BATCH_MS);
    },

    /** 每 100ms 至多一次 DOM 写入、一次至多 40 行：突发时单个任务有上限。 */
    flush() {
      this.timer = 0;
      const box = this.box();
      if (!box || document.hidden || !this.mounted || !this.follow) return;
      const batch = this.queue.splice(0, INSERT_MAX);
      if (batch.length) {
        $(".log-empty", box)?.remove();
        box.insertAdjacentHTML("beforeend", part(batch.map(logRow)));
        while (box.childElementCount > DOM_LINES) box.firstElementChild.remove();
        this.pin(box);
      }
      this.chrome();
      if (this.queue.length) this.timer = setTimeout(() => this.flush(), BATCH_MS);
    },

    stop() {
      this.resize?.disconnect();
      this.resize = null;
      this.source?.close();
      this.source = null;
      this.mounted = false;
      clearTimeout(this.timer);
      clearTimeout(this.retry);
      this.timer = 0;
      this.retry = 0;
      this.failures = 0;
      this.queue = [];
    },

    open() {
      const source = new EventSource(`/api/logs/stream?t=${encodeURIComponent(token)}`);
      this.source = source;
      source.addEventListener("open", () => {
        if (source !== this.source) return;
        this.failures = 0;
        this.setStatus("实时连接中");
      });
      source.addEventListener("snapshot", (event) => {
        if (source !== this.source) return;
        try {
          const data = JSON.parse(event.data);
          this.lines = data.lines.slice(-BUFFER);
          this.queue = [];
          if (this.follow || !$(".log-row", this.box())) this.paint();
          else this.chrome();
          if (data.dropped) this.setStatus(`实时连接中 · 追赶时跳过了 ${data.dropped} 条`);
        } catch {
          this.setStatus("这一批日志读取失败，刷新后重试");
        }
      });
      source.addEventListener("batch", (event) => {
        if (source !== this.source) return;
        try {
          this.push(JSON.parse(event.data).lines);
        } catch {
          /* 丢弃损坏的一批 */
        }
      });
      source.onerror = () => {
        if (source !== this.source) return;
        // CONNECTING：浏览器在自己重连；CLOSED：彻底断开，要我们来退避重试。
        if (source.readyState !== EventSource.CLOSED) {
          this.setStatus("连接中断，正在重连…");
          return;
        }
        source.close();
        this.source = null;
        const delay = Math.min(RETRY_MAX, RETRY_BASE * 2 ** Math.min(this.failures, 4));
        this.failures += 1;
        this.setStatus(`连接已断开，${Math.round(delay / 1000)} 秒后重试`);
        clearTimeout(this.retry);
        this.retry = setTimeout(() => {
          this.retry = 0;
          if (this.mounted && !document.hidden) this.open();
        }, delay);
      };
    },

    connect() {
      if (this.source && this.source.readyState !== EventSource.CLOSED) return;
      this.open();
    },

    mount() {
      const box = this.box();
      if (!box) {
        this.mounted = false;
        return;
      }
      this.mounted = true;
      if (this.follow || !box.dataset.ready) this.paint();
      box.dataset.ready = "true";
      this.top = box.scrollTop;
      box.onscroll = () => {
        const previous = this.top;
        this.top = box.scrollTop;
        if (Math.abs(box.scrollTop - previous) < 1) return;
        if (this.follow && box.scrollTop < previous && !this.stuck(box)) this.setFollow(false);
        else if (!this.follow && box.scrollTop > previous && this.stuck(box)) this.setFollow(true);
      };
      this.resize?.disconnect();
      this.resize = new ResizeObserver(() => {
        if (this.follow && box.isConnected && !document.hidden) this.pin(box);
      });
      this.resize.observe(box);
      if (!document.hidden) this.connect();
    },

    export() {
      const text = this.lines
        .filter((entry) => this.matches(entry))
        .map((entry) => `[${entry.at}] [${entry.level}] [${entry.target}] ${entry.text}`)
        .join("\n");
      const url = URL.createObjectURL(new Blob([`${text}\n`], { type: "text/plain;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = `acumen-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
  };

  function logsPage() {
    return h`<div class="page">
      ${pageHead("日志", "实时运行记录；往上翻会暂停跟随，回到底部继续。")}
      <section class="logs" aria-label="日志工作区">
        <div class="toolbar">
          <div class="search">${icon("search")}
            <input class="input" id="log-search" type="search" placeholder="搜索内容或来源" aria-label="搜索日志"
              autocomplete="off" value="${logs.query}">
          </div>
          <fieldset class="segmented" id="log-levels">
            <legend class="visually-hidden">日志级别</legend>
            ${LEVELS.map(
              ([value, label]) => h`<label class="segment"><input type="radio" name="log-level" value="${value}"${attr(logs.level === value, "checked")}>${label}</label>`
            )}
          </fieldset>
          <div class="actions">
            <button class="btn ${logs.follow ? "btn-tonal" : "btn-outlined"}" type="button" id="log-follow" aria-pressed="${bool(logs.follow)}">${icon("vertical_align_bottom")}跟随最新</button>
            <button class="icon-btn state" type="button" id="log-export" aria-label="导出当前筛选的日志" title="导出当前筛选的日志">${icon("download")}</button>
            <button class="icon-btn state" type="button" id="log-clear" aria-label="清空屏幕上的日志" title="清空屏幕上的日志">${icon("delete")}</button>
          </div>
        </div>
        <div class="log-meta"><span id="log-status" role="status">${logs.status}</span><span id="log-count"></span></div>
        <div class="log-frame">
          <div class="log" id="log-box" tabindex="0" role="region" aria-label="运行日志，最近 ${DOM_LINES} 条"></div>
          <button class="btn btn-filled jump" type="button" id="log-jump"${attr(logs.follow, "hidden")}>${icon("vertical_align_bottom")}<span>回到最新</span></button>
        </div>
      </section>
      <p class="note">页面切到后台时暂停接收，回来后自动补齐。完整日志请在本机运行 <code>acumen log</code>。</p>
    </div>`;
  }

  /* ==================== §11 设置 ==================== */

  const COMMANDS = ["list", "show ambient", "diff oai", "defaults stats"];
  const command = { output: "", history: [], busy: false };

  const SETTINGS_TABS = [
    { id: "general", href: "#/settings", label: "全局", glyph: "tune" },
    { id: "connections", href: "#/settings/connections", label: "连接", glyph: "hub" },
    { id: "system", href: "#/settings/system", label: "系统", glyph: "terminal" },
  ];

  function settingsTabs(active, connections = 0) {
    return h`<nav class="settings-tabs" aria-label="设置分区">${SETTINGS_TABS.map(
      (tab) => h`<a class="settings-tab state" href="${tab.href}"${attr(active === tab.id, 'aria-current="page"')}>
        ${icon(tab.glyph)}<span>${tab.label}</span>${tab.id === "connections" && connections ? h`<span class="settings-tab-count num">${connections}</span>` : ""}
      </a>`
    )}</nav>`;
  }

  function appearanceCard() {
    return h`<a class="appearance-card state" href="#/settings/appearance">
      <span class="tile tile-large" aria-hidden="true">${icon("palette")}</span>
      <span class="appearance-copy"><strong>外观与阅读习惯</strong><span>${appearance.summary()}</span></span>
      <span class="appearance-swatches" aria-hidden="true"><i></i><i></i><i></i></span>
      <span class="appearance-action">自定义${icon("chevron_right")}</span>
    </a>`;
  }

  /** 黑白名单在接口里是两个开关；对人则只是一种「接收范围」选择。
     白名单始终优先，因此三种展示模式不会丢掉任何语义，也不会让人误开两个开关。 */
  function globalScopeSummary(mode, blacklist, whitelist) {
    if (mode === "white") {
      return whitelist.length
        ? { tone: "ok", glyph: "check_circle-fill", title: `仅接收 ${whitelist.length} 个群的消息`, note: "其他群的消息会被完全忽略。" }
        : { tone: "warn", glyph: "warning-fill", title: "白名单为空，所有群都会被忽略", note: "请先添加至少一个群，再保存这个范围。" };
    }
    if (mode === "black") {
      return blacklist.length
        ? { tone: "ok", glyph: "check_circle-fill", title: `接收其他群的消息，排除 ${blacklist.length} 个群`, note: "黑名单里的群会被完全忽略。" }
        : { tone: "info", glyph: "info-fill", title: "黑名单为空，目前不过滤任何群", note: "添加群号后，才会排除对应群聊。" };
    }
    return { tone: "info", glyph: "groups", title: "接收所有群聊消息", note: "不使用全局群名单；插件自己的群规则仍然有效。" };
  }

  const scopeSummaryContent = (summary) =>
    h`${icon(summary.glyph)}<span class="scope-summary-copy"><strong>${summary.title}</strong><span>${summary.note}</span></span>`;

  async function settingsPage(route) {
    const [data, overview] = await Promise.all([api("/settings"), api("/overview").catch(() => null)]);
    const active = route.arg || "general";
    const filter = data.global_filter;
    const flags = { white: !!filter.enable_whitelist, black: !!filter.enable_blacklist };
    const mode = flags.white ? "white" : flags.black ? "black" : "all";
    const summary = globalScopeSummary(mode, filter.blacklist, filter.whitelist);
    const titles = {
      general: ["全局设置", "指令前缀、群聊接收范围与外观。"],
      connections: ["连接管理", "知微与聊天平台之间的实现端连接。"],
      system: ["系统维护", "维护命令、版本与本机信息。"],
    };
    const [title, subtitle] = titles[active] || titles.general;
    const head = pageHead(title, subtitle, {
      actions: active === "connections" ? h`<button class="btn btn-filled" type="button" data-add-bot>${icon("add")}添加连接</button>` : "",
    });

    if (active === "connections") {
      const online = overview?.bots.length || 0;
      const enabled = data.bots.filter((bot) => bot.enabled).length;
      const stopped = data.bots.length - enabled;
      return h`<div class="page settings-page">${head}${settingsTabs(active, data.bots.length)}
        <section class="section connections-section" aria-labelledby="bots-title">
          <div class="connection-overview">
            <span class="tile" aria-hidden="true">${icon("hub")}</span>
            <div><h2 id="bots-title">实现端连接</h2><p>${online ? `${online} 个账号当前在线` : "当前没有在线账号"} · ${enabled} 条已启用${stopped ? ` · ${stopped} 条已停用` : ""}</p></div>
            <span class="status status-${online ? "success" : "neutral"}">${online ? "已接入" : "待连接"}</span>
          </div>
          <p class="note">知微通过 Satori 实现端接入 QQ 或微信。地址通常在本机，例如 <code>http://127.0.0.1:3001</code>；修改连接后需要重启才能生效。</p>
          <div class="group bot-list" id="bot-list">
            ${data.bots.length ? data.bots.map((bot, index) => botForm(bot, String(index))) : h`<div class="card" id="bot-empty">${emptyState("还没有连接。点右上角的「添加连接」开始。", "hub")}</div>`}
          </div>
        </section>
      </div>`;
    }

    if (active === "system") {
      return h`<div class="page settings-page">${head}${settingsTabs(active, data.bots.length)}
        <div class="settings-system-grid">
          <section class="card command-card" aria-labelledby="command-title">
            <div class="settings-card-head"><span class="tile" aria-hidden="true">${icon("terminal")}</span><div><h2 id="command-title">维护命令</h2><p>与群里的 /ctl 相同，以维护者身份执行</p></div></div>
            <form class="command-form" id="command-form">
              <label class="visually-hidden" for="command-input">控制命令</label>
              <input class="input mono" id="command-input" autocomplete="off" spellcheck="false" autocapitalize="off"
                enterkeyhint="go" placeholder="输入命令" aria-describedby="command-hint">
              <button class="btn btn-filled" type="submit">${icon("play_arrow")}执行</button>
            </form>
            <p class="field-hint command-hint" id="command-hint">例如 <code>list</code>、<code>show ambient</code> 或 <code>diff oai</code>；也可以点下面的常用命令。</p>
            <div class="chips" role="group" aria-label="常用命令">
              ${[...new Set([...command.history, ...COMMANDS])].slice(0, 6).map(
                (item) => h`<button class="chip state" type="button" data-run="${item}">${item}</button>`
              )}
            </div>
            <pre class="code" id="command-output" role="status" tabindex="0" aria-label="命令回执">${command.output || "执行结果会显示在这里。"}</pre>
          </section>

          <section class="card about-card" aria-labelledby="about-title">
            <div class="settings-card-head"><span class="tile" aria-hidden="true">${icon("devices")}</span><div><h2 id="about-title">这台设备</h2><p>安装、版本与访问信息</p></div></div>
            <dl class="facts">
              ${overview
                ? h`<div class="fact"><dt>当前版本</dt><dd>${NAME} v${overview.app.version}</dd></div>
                    <div class="fact"><dt>控制台地址</dt><dd class="about-address"><code>${overview.console.address}</code><button class="icon-btn state" type="button" data-copy="${overview.console.address}" aria-label="复制控制台地址">${icon("content_copy")}</button></dd></div>
                    <div class="fact"><dt>本次启动</dt><dd><time>${overview.app.started}</time></dd></div>`
                : ""}
              <div class="fact"><dt>安装为应用</dt><dd>${installHint()}</dd></div>
            </dl>
            <div class="actions"><button class="btn btn-outlined" type="button" data-logout>${icon("logout")}在这台设备上退出</button></div>
          </section>
        </div>
      </div>`;
    }

    return h`<div class="page settings-page">${head}${settingsTabs(active, data.bots.length)}
      <section aria-label="外观设置">${appearanceCard()}</section>
      <form id="global-form" class="settings-form" autocomplete="off" novalidate>
        <section class="card settings-block" aria-labelledby="prefix-title">
          <div class="settings-card-head"><span class="tile" aria-hidden="true">${icon("quickreply")}</span><div><h2 id="prefix-title">指令与识别</h2><p>设置群里触发指令的方式，下一条消息起生效</p></div><span class="settings-effect">即时生效</span></div>
          <div class="setting">${listEditor({
            id: "g-prefix", type: "text", items: data.command_prefix, mode: "stage", name: "command_prefix", title: "指令前缀", describedby: "g-prefix-desc",
            head: h`<div class="field-text"><span class="field-label" id="g-prefix-label">指令前缀</span>
              <span class="field-hint" id="g-prefix-desc">例如 / 或 #。可以设置多个；全部移除后，指令无需前缀。</span></div>`,
          })}</div>
        </section>

        <section class="card settings-block scope-card" aria-labelledby="scope-title">
          <div class="settings-card-head"><span class="tile" aria-hidden="true">${icon("groups")}</span><div><h2 id="scope-title" tabindex="-1">群聊接收范围</h2><p>决定知微处理哪些群的消息，下一条消息起生效</p></div><span class="settings-effect">即时生效</span></div>
          <fieldset class="scope-fieldset">
            <legend class="field-label">接收范围</legend>
            <div class="segmented scope-segmented">
              ${[["all", "groups", "全部群聊"], ["black", "notifications_off", "排除名单"], ["white", "check_circle", "仅限名单"]].map(
                ([value, glyph, label]) => h`<label class="segment scope-segment"><input type="radio" name="scope_mode" value="${value}"${attr(mode === value, "checked")}>${icon(glyph)}<span>${label}</span></label>`
              )}
            </div>
          </fieldset>
          <div class="scope-summary" id="scope-summary" data-tone="${summary.tone}" role="status">${scopeSummaryContent(summary)}</div>
          <button class="switch" type="button" role="switch" data-form-switch name="enable_blacklist" aria-checked="${bool(filter.enable_blacklist)}" aria-label="启用群黑名单" hidden></button>
          <button class="switch" type="button" role="switch" data-form-switch name="enable_whitelist" aria-checked="${bool(filter.enable_whitelist)}" aria-label="启用群白名单" hidden></button>
          <div class="scope-picker" data-scope-panel="black"${attr(mode !== "black", "hidden")}>
            ${picker({ id: "g-black", label: "要排除的群", title: "黑名单", ids: filter.blacklist, mode: "stage", name: "blacklist" })}
          </div>
          <div class="scope-picker" data-scope-panel="white"${attr(mode !== "white", "hidden")}>
            ${picker({ id: "g-white", label: "允许处理的群", title: "白名单", ids: filter.whitelist, mode: "stage", name: "whitelist" })}
          </div>
          <p class="note">全局名单优先于插件处理。白名单模式下不检查黑名单；切换模式会保留另一张名单，之后可以随时切回。</p>
        </section>

        <details class="card settings-advanced">
          <summary><span class="tile" aria-hidden="true">${icon("web")}</span><span class="settings-advanced-copy"><strong>高级设置</strong><span>自定义浏览器可执行文件路径</span></span>${icon("keyboard_arrow_down", "settings-advanced-chevron")}</summary>
          <div class="settings-advanced-body">${textField("g-browser", "浏览器路径", "browser_path", data.browser_path, "留空时自动查找；修改后下次启动生效", "", true)}</div>
        </details>

        <div class="settings-savebar" id="global-savebar">
          <span class="settings-save-state" id="global-save-state" role="status" tabindex="-1">${icon("check_circle-fill")}<span>所有更改已保存</span></span>
          <div class="actions actions-end">
            <button class="btn btn-text" id="global-reset" type="button" data-global-reset hidden>撤销更改</button>
            <button class="btn btn-filled" type="submit" aria-keyshortcuts="Control+S Meta+S" disabled>${icon("check")}保存更改</button>
          </div>
        </div>
      </form>
    </div>`;
  }

  function syncGlobalScope(form) {
    if (!form) return;
    const mode = $('input[name="scope_mode"]:checked', form)?.value || "all";
    const white = $('[name="enable_whitelist"]', form);
    const black = $('[name="enable_blacklist"]', form);
    white?.setAttribute("aria-checked", bool(mode === "white"));
    black?.setAttribute("aria-checked", bool(mode === "black"));
    for (const panel of $$("[data-scope-panel]", form)) panel.hidden = panel.dataset.scopePanel !== mode;
    const ids = (name) => {
      const input = $(`.picker-value[name="${name}"]`, form);
      return input ? readIds(input.closest(".picker")) : [];
    };
    const blacklist = ids("blacklist");
    const whitelist = ids("whitelist");
    const summary = globalScopeSummary(mode, blacklist, whitelist);
    const node = $("#scope-summary", form);
    if (node) {
      node.dataset.tone = summary.tone;
      put(node, scopeSummaryContent(summary));
    }
    refreshStatuses(form);
  }

  function syncGlobalDraft(form = $("#global-form")) {
    if (!form) return;
    const busy = form.getAttribute("aria-busy") === "true";
    const dirty = formDirty(form, false);
    form.toggleAttribute("data-dirty", dirty);
    const submit = $('[type="submit"]', form);
    if (submit) submit.disabled = busy || !dirty;
    const reset = $("[data-global-reset]", form);
    if (reset) reset.hidden = !dirty || busy;
    const state = $("#global-save-state", form);
    if (state) {
      // 只在状态真的变了时重写这一格：每敲一个字都换一次 innerHTML，
      // 读屏会把同一句「有未保存的更改」反复念出来。
      const phase = busy ? "saving" : dirty ? "dirty" : "clean";
      if (state.dataset.phase !== phase) {
        state.dataset.phase = phase;
        state.dataset.dirty = bool(dirty);
        put(state, busy
          ? h`${icon("schedule")}<span>正在保存更改…</span>`
          : dirty
            ? h`${icon("edit")}<span>有未保存的更改</span>`
            : h`${icon("check_circle-fill")}<span>所有更改已保存</span>`);
      }
    }
  }

  function installHint() {
    const standalone = matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
    if (standalone) return "已作为应用打开。";
    if (/iPhone|iPad|iPod/.test(navigator.userAgent)) return "在 Safari 中点「分享」，再选「添加到主屏幕」。";
    if (/Android/.test(navigator.userAgent)) return "在浏览器菜单中选择「安装应用」或「添加到主屏幕」。";
    return "在浏览器地址栏右侧或菜单中选择「安装」。";
  }

  const textField = (id, label, name, value, hint, extra = "", tile = false) =>
    h`<div class="field${tile ? " setting" : ""}">
      <label class="field-label" for="${id}">${label}</label>
      <input class="input mono" id="${id}" name="${name}" value="${value}" spellcheck="false" autocapitalize="off"
        autocomplete="off" aria-describedby="${id}-hint"${raw(extra)}>
      <span class="field-hint" id="${id}-hint">${hint}</span>
    </div>`;

  function botForm(bot, index) {
    const id = `bot-${index === "" ? "new" : index}`;
    const isNew = index === "";
    const known = ["satori", "console"];
    const protocols = known.includes(bot.protocol) ? known : [...known, bot.protocol];
    const follows = { satori_qq: "知弦（QQ）", satori_wx: "知言（微信）" }[bot.module] || bot.module;
    const title = isNew ? "新连接" : bot.module ? follows : `连接 ${Number(index) + 1}`;
    const subtitle = bot.module ? `跟随${follows}模块` : bot.url || (bot.protocol === "console" ? "本地终端" : "尚未填写实现端地址");
    return h`<form class="bot" data-bot="${index}"${attr(isNew, "data-open")}${attr(bot.module, "data-follow")} aria-labelledby="${id}-title" autocomplete="off" novalidate>
      <div class="bot-head">
        <button class="bot-summary" type="button" data-bot-toggle aria-expanded="${bool(isNew)}" aria-controls="${id}-body">
          <span class="tile" aria-hidden="true">${icon("hub")}</span>
          <span class="bot-summary-copy"><span class="bot-title" id="${id}-title">${title}${bot.has_token ? h`<span class="status">已设令牌</span>` : ""}</span><span class="bot-subtitle">${subtitle}</span></span>
        </button>
        <div class="bot-enable">
          <span class="field-label" id="${id}-on">启用</span>
          <button class="switch" type="button" role="switch" data-form-switch name="enabled" aria-checked="${bool(bot.enabled)}" aria-labelledby="${id}-on"></button>
        </div>
        <span class="bot-chevron" aria-hidden="true">${icon("keyboard_arrow_down")}</span>
      </div>
      <div class="bot-body" id="${id}-body"${attr(!isNew, "hidden")}>
        <div class="form-grid">
          <div class="field">
            <label class="field-label" for="${id}-protocol">协议</label>
            <span class="select"><select id="${id}-protocol" name="protocol">
              ${protocols.map((p) => h`<option value="${p}"${attr(p === bot.protocol, "selected")}>${p === "satori" ? "Satori" : p === "console" ? "本地终端（测试用）" : p}</option>`)}
            </select>${icon("keyboard_arrow_down")}</span>
          </div>
          ${textField(`${id}-url`, "实现端地址", "url", bot.url,
            bot.module ? `留空即跟随${follows}模块里设的端口与令牌` : "Satori 必填，例如 http://127.0.0.1:3001", ' inputmode="url"')}
          <div class="field span-all">
            <label class="field-label" for="${id}-token">访问令牌</label>
            <div class="reveal"><input class="input mono" id="${id}-token" name="access_token" type="password" autocomplete="off" spellcheck="false"
              data-1p-ignore data-lpignore="true" data-bwignore="true"
              placeholder="${bot.has_token ? "已设置；留空保持不变" : bot.module ? "留空即用模块里的令牌" : "留空表示不鉴权"}">
              <button class="icon-btn state" type="button" data-reveal="#${id}-token" aria-pressed="false" aria-label="显示访问令牌">${icon("visibility")}</button></div>
            ${bot.has_token ? h`<span class="field-hint">留空会保留原令牌；需要更换时再输入新值。</span>` : ""}
          </div>
          ${bot.has_token
            ? h`<div class="field span-all">
                <div class="toggle-row"><span class="field-label" id="${id}-clear">清除已设的令牌</span>
                  <button class="switch" type="button" role="switch" data-form-switch name="clear_token" aria-checked="false"
                    aria-labelledby="${id}-clear" aria-describedby="${id}-clear-hint"></button></div>
                <span class="field-hint" id="${id}-clear-hint">打开后保存，这条连接改为不鉴权。留空只是不改动。</span>
              </div>`
            : ""}
        </div>
        <div class="actions bot-actions">
          <button class="btn btn-filled" type="submit">${icon("check")}保存连接</button>
          <button class="btn btn-text btn-danger" type="button" data-drop-bot="${index}">${icon("delete")}${isNew ? "放弃" : "删除连接"}</button>
          <span class="bot-effect">保存后，重启知微生效</span>
        </div>
      </div>
    </form>`;
  }

  function botPayload(form) {
    const value = (name) => form.elements[name]?.value ?? "";
    const access = tokenValue(form);
    const clear = $('[name="clear_token"]', form)?.getAttribute("aria-checked") === "true";
    return {
      index: form.dataset.bot === "" ? undefined : Number(form.dataset.bot),
      enabled: $('[name="enabled"]', form)?.getAttribute("aria-checked") === "true",
      protocol: value("protocol"),
      url: value("url"),
      ...(clear ? { clear_token: true } : access.trim() ? { access_token: access } : {}),
    };
  }

  function globalPayload(form) {
    const value = (name) => form.elements[name]?.value ?? "";
    // 输入框里还悬着没加进去的群号，保存前先收进名单；收不进去就停在这里，错误已写在那一格下面。
    for (const root of $$(".picker", form)) {
      if (!pickerSubmitText(root)) {
        // 提交期间整张表单的控件是停用的，焦点要等它们恢复后再还给出问题的那一格（见提交处）。
        throw Object.assign(new Error("名单输入框里还有没加进去的内容"), { refocus: pickerAdd(root) });
      }
    }
    // 前缀框里还悬着一项没加进去：一并收下。
    for (const root of $$(".tags", form)) {
      if (!tagsSubmit(root)) throw Object.assign(new Error("列表输入框里还有没加进去的内容"), { refocus: tagsInput(root) });
    }
    const groups = (name) => readIds($(`.picker-value[name="${name}"]`, form).closest(".picker"));
    const flag = (name) => $(`[name="${name}"]`, form)?.getAttribute("aria-checked") === "true";
    return {
      command_prefix: readList($(".tags-value[name='command_prefix']", form).closest(".tags")).map(String),
      browser_path: value("browser_path"),
      global_filter: {
        enable_blacklist: flag("enable_blacklist"),
        blacklist: groups("blacklist"),
        enable_whitelist: flag("enable_whitelist"),
        whitelist: groups("whitelist"),
      },
    };
  }

  async function submitWith(form, work) {
    const button = $('[type="submit"]', form);
    if (button?.getAttribute("aria-busy") === "true") return;
    button?.setAttribute("aria-busy", "true");
    form.setAttribute("aria-busy", "true");
    const controls = $$("input, select, textarea, button", form).filter(control => !control.disabled);
    for (const control of controls) control.disabled = true;
    try {
      await work();
    } finally {
      button?.removeAttribute("aria-busy");
      form.removeAttribute("aria-busy");
      for (const control of controls) control.disabled = false;
    }
  }

  async function runCommand(input) {
    const text = input.trim();
    if (!text || command.busy) return;
    command.busy = true;
    const output = $("#command-output");
    const button = $('#command-form [type="submit"]');
    button?.setAttribute("aria-busy", "true");
    if (output) output.textContent = "执行中…";
    try {
      const reply = await api("/command", { method: "POST", body: { input: text } });
      command.output = reply.message;
      command.history = [text, ...command.history.filter((item) => item !== text)].slice(0, 3);
    } catch (error) {
      if (error.status === 401) return;
      command.output = error.message;
    } finally {
      command.busy = false;
      button?.removeAttribute("aria-busy");
      const box = $("#command-output");
      if (box) box.textContent = command.output;
    }
  }

  /* ==================== §11a 名单编辑器 ==================== */
  /* 群名单（全局黑白名单、各插件的 channel 与「开启的群」）和私聊用户名单（资讯推送的私聊目标、
     管理员）说到底是一串 ID。手打 JSON 引号、数逗号、记账号都不是人该干的事：这里改成从已加入的群
     / 好友里点选、粘贴一串自动拆开、一键移除并能撤销。名单本身仍是字符串数组，写入口径一个字都没变。
     两种名单只是名册和措辞不同（KINDS），其余完全共用。 */

  const ID_SPLIT = /[\s,，、;；|]+/u;
  const ID_EDGE = /^["'“”‘’「」『』《》()（）[\]【】{}<>]+|["'“”‘’「」『』《》()（）[\]【】{}<>]+$/gu;
  const ID_SHAPE = /^[\w-][\w.@:+-]{0,127}$/;

  /** 从一段文字里拆出群号：逗号、顿号、分号、空白、换行都当分隔；两端的引号括号剥掉，
   *  整段是 JSON 数组也认得；全角数字先折成半角。认不出的原样放进 `rejected`，由调用方说明。
   *  微信的群号是 `…@chatroom`、私聊账号可以是 `wxid_…` / `gh_…`，所以只管「像不像一个 ID」，不限数字。 */
  function parseIds(text) {
    const ids = [];
    const rejected = [];
    const clean = String(text).normalize("NFKC").replace(/[​-‍⁠﻿]/g, "");
    for (const piece of clean.split(ID_SPLIT)) {
      const token = piece.replace(ID_EDGE, "");
      if (!token) continue;
      if (!ID_SHAPE.test(token)) rejected.push(token);
      else if (!ids.includes(token)) ids.push(token);
    }
    return { ids, rejected };
  }

  const sameIds = (a, b) => a.length === b.length && a.every((id, index) => id === b[index]);
  // Satori 里 QQ 的平台名是 red（QQ NT 协议），微信是 wechat；认不得的原样大写显示。
  const PLATFORM_NAMES = { red: "QQ", qq: "QQ", wechat: "微信", wx: "微信", console: "控制台" };

  /** 一份名册（ID → 名字）：已加入的群，或好友。名单里存的是 ID，人要看的是名字：页面一出现名单就去
   *  问后端要一次，回来后把 ID 补上名字；问不到（实现端没连上）也不影响手输 ID。 */
  const makeRoster = (endpoint) => ({
    list: [],
    byId: new Map(),
    platforms: 0,
    failed: 0,
    loaded: false,
    error: false,
    at: 0,
    pending: null,
    load(force = false) {
      if (this.pending) return this.pending;
      if (!force && this.loaded && !this.error && Date.now() - this.at < 60000) return Promise.resolve(this);
      this.pending = api(`${endpoint}${force ? "?refresh=1" : ""}`)
        .then((data) => {
          this.list = Array.isArray(data.items) ? data.items : [];
          this.byId = new Map();
          for (const entry of this.list) if (!this.byId.has(entry.id)) this.byId.set(entry.id, entry);
          this.platforms = new Set(this.list.map((entry) => entry.platform)).size;
          this.failed = Number(data.failed) || 0;
          this.loaded = true;
          this.error = false;
          this.at = Date.now();
        })
        .catch((error) => {
          if (error?.status !== 401) this.error = true;
        })
        .then(() => {
          this.pending = null;
          return this;
        });
      return this.pending;
    },
  });

  /** 两种名册与它们的措辞。键是 `data-kind`：group 是群号名单，user 是私聊对象名单。 */
  const KINDS = {
    group: {
      roster: makeRoster("/groups"),
      icon: "groups",
      idNoun: "群号",
      placeholder: "输入或粘贴群号，回车添加",
      pick: "从已加入的群里选",
      sheet: "选择群",
      search: "搜索群名或群号",
      aria: "已选的群",
      listAria: "群列表",
      total: (n) => `共 ${n} 个群`,
      stale: "不在已加入的群里",
      offer: "不在已加入的群里，按群号直接添加",
      reading: "正在读取已加入的群…",
      nomatch: "没有匹配的群。可以换个关键词，或直接输入群号。",
      none: "没有读到已加入的群。账号可能还没连上实现端；也可以回去直接输入群号。",
      stale_read: "没能读到最新的群，",
      notId: (text) => `「${text}」不像群号。群号是一串数字；想按群名找，点下面的「从已加入的群里选」。`,
    },
    user: {
      roster: makeRoster("/friends"),
      icon: "person",
      idNoun: "账号",
      placeholder: "输入或粘贴 QQ 号或微信号，回车添加",
      pick: "从好友里选",
      sheet: "选择好友",
      search: "搜索昵称、备注或账号",
      aria: "已选的用户",
      listAria: "好友列表",
      total: (n) => `共 ${n} 位好友`,
      stale: "不在好友列表里",
      offer: "不在好友列表里，按账号直接添加",
      reading: "正在读取好友…",
      nomatch: "没有匹配的好友。可以换个关键词，或直接输入账号。",
      none: "没有读到好友。账号可能还没连上实现端；也可以回去直接输入账号。",
      stale_read: "没能读到最新的好友，",
      notId: (text) => `「${text}」不像账号。QQ 号是一串数字，微信号形如 wxid_…；想按昵称找，点下面的「从好友里选」。`,
    },
  };

  const kindOf = (node) => (node.dataset?.kind === "user" ? "user" : "group");
  const entryName = (kind, id) => KINDS[kind].roster.byId.get(id)?.name || "";
  /** 「白虎（175131947）」；名字未知时就是 ID 本身。给读屏名与提示文字用。 */
  const entryTitle = (kind, id) => (entryName(kind, id) ? `${entryName(kind, id)}（${id}）` : id);
  /** 搜索认名字、别名（昵称 / 备注）与 ID。 */
  const searchable = (entry) => `${entry.name} ${entry.alias ?? ""} ${entry.id}`.toLowerCase();
  const platformTag = (kind, id) => {
    const { roster } = KINDS[kind];
    const entry = roster.byId.get(id);
    // 只连了一个平台时标出来是噪音；两个平台都在线才有区分的价值。
    return entry && roster.platforms > 1 ? h`<span class="tag">${PLATFORM_NAMES[entry.platform] ?? String(entry.platform).toUpperCase()}</span>` : "";
  };

  /** 名单当前的效果，用人话说。全局两张名单是互相牵连的（白名单开着就不查黑名单），
   *  所以要带上两个开关的状态；返回 [语气, 文字]。 */
  const STATUS = {
    "global-white": (n, on) =>
      !on.white
        ? ["info", "未启用，名单暂不生效。"]
        : n
          ? ["ok", `只处理这 ${n} 个群的消息，其余的群完全忽略。`]
          : ["warn", "名单是空的：现在所有群都会被忽略，知微不处理任何群的消息。"],
    "global-black": (n, on) =>
      on.white
        ? ["info", "白名单已启用，此时不检查黑名单。"]
        : !on.black
          ? ["info", "未启用，名单暂不生效。"]
          : n
            ? ["ok", `这 ${n} 个群的消息会被完全忽略。`]
            : ["info", "名单是空的，没有群被排除。"],
    "channel-white": (n) => (n ? ["ok", `只在这 ${n} 个群里生效。`] : ["info", "名单是空的：所有群都生效。"]),
    "channel-black": (n) => (n ? ["ok", `这 ${n} 个群不生效，且优先于白名单。`] : ["info", "名单是空的：没有群被排除。"]),
    "rule-groups": (n) => (n ? ["ok", `只在这 ${n} 个群里回。`] : ["info", "名单是空的：所有群都回（共用规则）。"]),
    "rule-exclude": (n) => (n ? ["ok", `这 ${n} 个群不回，别的群照回。`] : ["info", "没有不回的群。"]),
  };

  const statusLine = (role, n, on) => {
    const [tone, text] = STATUS[role](n, on);
    return { tone, html: h`${icon(tone === "warn" ? "warning-fill" : tone === "ok" ? "check_circle-fill" : "info-fill")}<span>${text}</span>` };
  };

  /** 一行：名字为主，ID（与别名）为辅；名册读到了却没有这一项就标出来。 */
  const pickerRow = (kind) => (id) => {
    const { roster, stale } = KINDS[kind];
    const entry = roster.byId.get(id);
    const missing = roster.loaded && roster.list.length > 0 && !entry;
    return h`<li class="item picker-row">
      <span class="item-leading" aria-hidden="true">${entry?.name ? [...entry.name][0] : "#"}</span>
      <div class="item-content">
        <span class="item-title">${entry?.name || h`<span class="mono">${id}</span>`}${platformTag(kind, id)}</span>
        ${entry?.name || missing
          ? h`<span class="item-sub">${entry?.name ? h`<span class="mono">${id}</span>${entry.alias ? ` · ${entry.alias}` : ""}` : stale}</span>`
          : ""}
      </div>
      <div class="item-trailing">
        <button class="icon-btn state" type="button" data-picker-remove="${id}" aria-label="移除 ${entryTitle(kind, id)}">${icon("close")}</button>
      </div>
    </li>`;
  };

  /** 名单编辑器。`kind`：group = 群号名单，user = 私聊对象名单。
   *  `mode`：save = 每次改动立刻写进插件配置；stage = 只改页面上的草稿，随表单一起保存。
   *  真正的值放在一个藏起来的文本框里（JSON 数组），其余都是它的视图。
   *  不用 type=hidden：它的 value 与 defaultValue 是同一个特性，分不出「改没改」。 */
  function picker({ id, label, title = "", kind = "group", ids, mode, name = "", path = "", role = "", flags = {} }) {
    const text = KINDS[kind];
    void text.roster.load().then(paintPickers);
    const list = [...new Set(ids.map(String))];
    const status = role ? statusLine(role, list.length, flags) : null;
    return h`<div class="field picker" role="group" aria-labelledby="${id}-label" data-picker="${mode}" data-kind="${kind}" data-role="${role}" data-title="${title}">
      <div class="picker-head">
        <span class="field-label" id="${id}-label">${label}</span>
        <span class="picker-count num">${list.length ? `${list.length} 个` : ""}</span>
      </div>
      <ul class="group picker-list" aria-label="${text.aria}">${list.map(pickerRow(kind))}</ul>
      ${status ? h`<p class="picker-status" data-tone="${status.tone}">${status.html}</p>` : ""}
      <div class="picker-controls">
        <div class="picker-add">
          <div class="picker-input">
            <input class="input mono" id="${id}-add" type="text" inputmode="text" enterkeyhint="done" spellcheck="false"
              autocapitalize="off" autocomplete="off" placeholder="${text.placeholder}" aria-label="添加${text.idNoun}" aria-describedby="${id}-hint">
          </div>
          <button class="btn btn-outlined" type="button" data-picker-add disabled>添加</button>
        </div>
        <button class="btn btn-text picker-choose" type="button" data-picker-open>${icon(text.icon)}${text.pick}</button>
      </div>
      <span class="field-hint picker-hint" id="${id}-hint">可以整段粘贴：逗号、空格、换行、引号都行。</span>
      <input class="picker-value" id="${id}" type="text" hidden tabindex="-1" autocomplete="off" value="${JSON.stringify(list)}"
        ${name ? h`name="${name}"` : ""} ${path ? h`data-path="${path}" data-kind="list"` : ""}>
      <span class="visually-hidden picker-live" role="status"></span>
    </div>`;
  }

  const pickerValue = (root) => $(".picker-value", root);
  const pickerAdd = (root) => $(".picker-add input", root);

  function readIds(root) {
    try {
      const value = JSON.parse(pickerValue(root).value);
      return Array.isArray(value) ? value.map(String) : [];
    } catch {
      return [];
    }
  }

  function paintStatus(root, ids = readIds(root)) {
    const node = $(".picker-status", root);
    if (!node || !STATUS[root.dataset.role]) return;
    const form = root.closest("form");
    const on = (name) => !!form && $(`[name="${name}"]`, form)?.getAttribute("aria-checked") === "true";
    const { tone, html } = statusLine(root.dataset.role, ids.length, { white: on("enable_whitelist"), black: on("enable_blacklist") });
    node.dataset.tone = tone;
    put(node, html);
  }

  /** 重画一张名单。`late`：群名是后来才到的——那时若焦点正落在某一行的按钮上就先别动它，
   *  免得一次重画把键盘用户的焦点抢走。 */
  function paintPicker(root, { late = false } = {}) {
    const ids = readIds(root);
    const list = $(".picker-list", root);
    if (!(late && list.contains(document.activeElement))) put(list, ids.map(pickerRow(kindOf(root))));
    $(".picker-count", root).textContent = ids.length ? `${ids.length} 个` : "";
    paintStatus(root, ids);
  }

  const paintPickers = () => {
    for (const root of $$(".picker")) paintPicker(root, { late: true });
  };

  const refreshStatuses = (form) => {
    for (const root of $$(".picker", form)) paintStatus(root);
  };

  /** 把新名单写进视图。「是不是亲手改的」只认这里（见 typed）。 */
  function applyIds(root, ids) {
    const input = pickerValue(root);
    input.value = JSON.stringify(ids);
    typed.add(input);
    paintPicker(root);
  }

  function describeChange(kind, prev, next) {
    const added = next.filter((id) => !prev.includes(id));
    const removed = prev.filter((id) => !next.includes(id));
    const say = (verb, ids) => (ids.length === 1 ? `${verb}「${entryName(kind, ids[0]) || ids[0]}」` : `${verb} ${ids.length} 个`);
    return [added.length && say("加入", added), removed.length && say("移除", removed)].filter(Boolean).join("，");
  }

  /** 改名单的唯一入口。先改页面，再按模式处理：save 立刻落盘，stage 只留草稿。
   *  每次改动都能撤销（撤销本身不再给撤销）。 */
  async function pickerEdit(root, next, { lead = "", undoable = true } = {}) {
    const prev = readIds(root);
    if (sameIds(prev, next)) return;
    const said = lead || `已${describeChange(kindOf(root), prev, next)}`;
    applyIds(root, next);
    const undo = undoable
      ? {
          label: "撤销",
          run: () => {
            void pickerEdit(root, prev, { lead: "已撤销", undoable: false });
            $(".picker-choose", root)?.focus({ preventScroll: true });
          },
        }
      : null;
    if (root.dataset.picker === "save") return pickerSave(root, said, undo);
    if (root.dataset.picker === "emit") return void root.dispatchEvent(new CustomEvent("listchange", { bubbles: true, detail: { said, undo } }));
    const draft = root.closest("#global-form");
    if (draft) {
      syncGlobalScope(draft);
      syncGlobalDraft(draft);
    }
    $(".picker-live", root).textContent = `${said}，现在共 ${next.length} 个。`;
    // 草稿里加一个群不必打扰；移除才值得留一条能撤销的提示。
    if (next.length < prev.length || !undoable) snackbar(`${said}。`, "info", undo);
  }

  /** 把名单写进插件配置（见 saveList）。 */
  const pickerSave = (root, said, undo) => saveList(root, pickerValue(root), pickerAdd(root), () => paintPicker(root), said, undo);

  async function pickerRemove(button) {
    const root = button.closest(".picker");
    const id = button.dataset.pickerRemove;
    const ids = readIds(root);
    const index = ids.indexOf(id);
    if (index < 0) return;
    // 同步段先跑完（名单已重画），再把焦点交给顶上来的那一行：键盘用户连着删不必重新找位置。
    const done = pickerEdit(root, ids.filter((item) => item !== id));
    const buttons = $$("[data-picker-remove]", root);
    (buttons[Math.min(index, buttons.length - 1)] ?? pickerAdd(root))?.focus({ preventScroll: true });
    await done;
  }

  const syncAddButton = (root) => {
    const button = $("[data-picker-add]", root);
    if (button) button.disabled = !pickerAdd(root).value.trim();
  };

  /** 输入框里的文字变成群号加进名单。`explicit`：用户按了回车或「添加」——文字不像群号时，
   *  若它能对上某个群名就替他打开选群面板，而不是只丢一句报错。离开输入框时也走这里（离开即保存）。
   *  返回输入框里是否已经没有悬着的内容。 */
  function pickerSubmitText(root, { explicit = false } = {}) {
    const add = pickerAdd(root);
    const text = add.value;
    if (!text.trim()) return true;
    const kind = kindOf(root);
    const { roster, idNoun, notId } = KINDS[kind];
    const { ids, rejected } = parseIds(text);
    if (!ids.length) {
      const query = text.trim().toLowerCase();
      if (explicit && roster.list.some((entry) => searchable(entry).includes(query))) {
        // 字交给面板的搜索框去用，这边清空：免得面板收起后输入框里还留着一句过期的报错。
        add.value = "";
        syncAddButton(root);
        clearFieldError(add);
        void pickerChoose(root, text.trim());
        return false;
      }
      fieldError(add, notId(rejected[0] ?? text.trim()));
      return false;
    }
    const current = readIds(root);
    const fresh = ids.filter((id) => !current.includes(id));
    add.value = rejected.join(" ");
    syncAddButton(root);
    clearFieldError(add);
    if (rejected.length) fieldError(add, `「${rejected.join("、")}」不像${idNoun}，没有加；其余的已处理。`);
    if (!fresh.length) {
      snackbar(ids.length === 1 ? `「${entryName(kind, ids[0]) || ids[0]}」已经在名单里了。` : "这些都已经在名单里了。");
    } else void pickerEdit(root, [...current, ...fresh]);
    return !rejected.length;
  }

  async function pickerChoose(root, query = "") {
    if ($("#sheet").open) return;
    const picked = await openSheet({ selected: readIds(root), query, title: root.dataset.title, kind: kindOf(root) });
    if (picked) await pickerEdit(root, picked);
  }

  /** 选择面板：已加入的群（或好友）逐行勾选，搜索名字、别名或 ID。「完成」才一并生效，取消 / Esc 什么都不改。
   *  勾选状态只存在这里的集合里，重画列表（搜索、刷新、名字晚到）不会丢。 */
  function openSheet({ selected, query = "", title = "", kind = "group" }) {
    const text = KINDS[kind];
    const { roster } = text;
    const dialog = $("#sheet");
    const opener = document.activeElement;
    const initial = new Set(selected);
    const picked = new Set(selected);
    const order = [...selected];
    const stop = new AbortController();
    let search = query;

    const visible = () => {
      const terms = search.normalize("NFKC").toLowerCase().split(/\s+/).filter(Boolean);
      const hit = (entry) => terms.every((term) => searchable(entry).includes(term));
      // 进来时已在名单里的排最前（名册里没有的也要能取消勾选）；之后顺序不再变，免得点一下行就跳走。
      const lead = [...initial].map((id) => roster.byId.get(id) ?? { id, name: "", platform: "" });
      const rest = roster.list.filter((entry) => !initial.has(entry.id));
      return [...lead, ...rest].filter(hit);
    };
    const option = (entry) => h`<label class="item option">
      <input class="check" type="checkbox" value="${entry.id}"${attr(picked.has(entry.id), "checked")}>
      <span class="item-content">
        <span class="item-title">${entry.name || h`<span class="mono">${entry.id}</span>`}${platformTag(kind, entry.id)}</span>
        <span class="item-sub">${entry.name ? h`<span class="mono">${entry.id}</span>${entry.alias ? ` · ${entry.alias}` : ""}` : text.stale}</span>
      </span>
    </label>`;

    const count = (shown) => {
      const total = roster.list.length;
      const scope = search.trim() ? `显示 ${shown} 个` : text.total(total);
      $("#sheet-count", dialog).textContent = `已选 ${picked.size} 个${total ? ` · ${scope}` : ""}`;
    };
    const paint = () => {
      const body = $("#sheet-body", dialog);
      const top = body.scrollTop;
      const rows = visible();
      const asId = parseIds(search);
      const typedId = asId.ids.length === 1 && !asId.rejected.length ? asId.ids[0] : "";
      const offer = typedId && !picked.has(typedId) && !roster.byId.has(typedId) && !initial.has(typedId);
      const waiting = !roster.loaded && roster.pending;
      put(
        body,
        h`${roster.error ? h`<p class="note sheet-note" role="alert">${icon("error-fill")}<span>${text.stale_read}${roster.list.length ? "下面是上次读到的。" : `可以直接回去输入${text.idNoun}。`}</span></p>` : ""}
        ${roster.failed ? h`<p class="note sheet-note">${icon("info-fill")}<span>有 ${roster.failed} 条连接没读到，这张表可能不全。</span></p>` : ""}
        ${rows.length || offer
          ? h`<div class="group" role="group" aria-label="${text.listAria}">
              ${offer ? h`<button class="item option option-add" type="button" data-sheet-add="${typedId}">
                <span class="item-leading" aria-hidden="true">${icon("add")}</span>
                <span class="item-content"><span class="item-title">添加 <span class="mono">${typedId}</span></span>
                <span class="item-sub">${text.offer}</span></span></button>` : ""}
              ${rows.map(option)}
            </div>`
          : emptyState(waiting ? text.reading : roster.list.length ? text.nomatch : text.none, waiting ? text.icon : "inbox")}`
      );
      body.scrollTop = top;
      count(rows.length);
    };

    put(
      dialog,
      h`<div class="sheet-head">
          <h2 class="dialog-title" id="sheet-title" tabindex="-1">${text.sheet}${title ? ` · ${title}` : ""}</h2>
          <button class="icon-btn state" type="button" data-sheet="cancel" aria-label="关闭，不保存">${icon("close")}</button>
        </div>
        <div class="sheet-tools">
          <div class="search">${icon("search")}
            <input class="input" id="sheet-search" type="search" value="${query}" placeholder="${text.search}" aria-label="${text.search}"
              autocomplete="off" spellcheck="false" enterkeyhint="search">
          </div>
          <div class="sheet-meta">
            <span id="sheet-count" role="status"></span>
            <button class="btn btn-text" type="button" data-sheet="refresh">${icon("refresh")}刷新</button>
          </div>
        </div>
        <div class="sheet-body" id="sheet-body" tabindex="-1"></div>
        <div class="actions actions-end sheet-foot">
          <button class="btn btn-text" type="button" data-sheet="cancel">取消</button>
          <button class="btn btn-filled" type="button" data-sheet="done">完成</button>
        </div>`
    );

    const { signal } = stop;
    dialog.addEventListener(
      "input",
      (event) => {
        if (event.target.id !== "sheet-search") return;
        search = event.target.value;
        paint();
        $("#sheet-body", dialog).scrollTop = 0;
      },
      { signal }
    );
    dialog.addEventListener(
      "change",
      (event) => {
        const box = event.target;
        if (!box.matches?.(".check")) return;
        if (box.checked) {
          picked.add(box.value);
          if (!order.includes(box.value)) order.push(box.value);
        } else picked.delete(box.value);
        count(visible().length);
      },
      { signal }
    );
    dialog.addEventListener(
      "click",
      async (event) => {
        const hit = event.target.closest?.("[data-sheet], [data-sheet-add]");
        if (!hit) return;
        if (hit.dataset.sheetAdd) {
          picked.add(hit.dataset.sheetAdd);
          if (!order.includes(hit.dataset.sheetAdd)) order.push(hit.dataset.sheetAdd);
          search = "";
          $("#sheet-search", dialog).value = "";
          paint();
          $("#sheet-search", dialog).focus();
        } else if (hit.dataset.sheet === "refresh") {
          hit.setAttribute("aria-busy", "true");
          await roster.load(true);
          hit.removeAttribute("aria-busy");
          if (dialog.open) paint();
        } else dialog.close(hit.dataset.sheet);
      },
      { signal }
    );
    dialog.addEventListener("keydown", (event) => {
      // 搜索框里回车不该提交任何东西，也不要把面板关掉。
      if (event.key === "Enter" && event.target.id === "sheet-search") event.preventDefault();
    }, { signal });

    return new Promise((resolve) => {
      dialog.addEventListener(
        "close",
        () => {
          stop.abort();
          if (opener instanceof HTMLElement && opener.isConnected) opener.focus({ preventScroll: true });
          resolve(dialog.returnValue === "done" ? order.filter((id) => picked.has(id)) : null);
        },
        { once: true }
      );
      dialog.returnValue = "";
      paint();
      dialog.showModal();
      // 触屏上一弹出就聚焦搜索框会把键盘顶起来盖住列表；只给精确指针自动聚焦。
      (matchMedia("(pointer: fine)").matches ? $("#sheet-search", dialog) : $("#sheet-title", dialog)).focus({ preventScroll: true });
      void roster.load().then(() => {
        if (dialog.open) paint();
      });
    });
  }

  /* ==================== §11b 列表编辑器 ==================== */
  /* 不是 ID 的列表：前缀、别名、关键词、模型关键字、时间点、星期……旧版是一行 JSON，
     要人自己写引号、数逗号。这里每一项是一枚输入芯片（M3 input chip）：点一下改、✕ 移走并能撤销、
     拖动或 Alt+方向键排序、回车添加、整段粘贴按行拆开。时间点用系统的时间选择器并自动排好序，
     星期是七枚开关芯片。写入口径与名单编辑器相同（save 立刻落盘、stage 随表单保存），值仍是原样的数组。 */

  const WEEKDAYS = ["一", "二", "三", "四", "五", "六", "日"];
  const LIST_TEXT = {
    text: { placeholder: "输入一项，回车添加", hint: "粘贴多行会拆成多项。点一项可修改；按住拖动或 Alt+方向键调整顺序。" },
    number: { placeholder: "输入数字，回车添加", hint: "可以一次输入一串，用空格或逗号隔开。点一项可修改。" },
    time: { placeholder: "", hint: "选好时间点再点「添加」，按先后自动排序。点一项可修改。" },
  };

  /** 列表是哪一种：image / weekday / time / number / text；认不出（对象、混着类型、带隐藏凭据）返回空串，仍走 JSON。
   *  空列表看默认值的元素类型，默认值也空就当文字。`images` 是配图，走图库编辑器。 */
  function listShape(key, value, fallback) {
    if (value.some((item) => typeof item === "string" && item.includes(HIDDEN))) return "";
    if (key === "images" && value.every((item) => typeof item === "string")) return "image";
    const sample = value.length ? value : Array.isArray(fallback) ? fallback : [];
    if (sample.length && sample.every((item) => typeof item === "number")) {
      return key === "weekdays" && sample.every((day) => Number.isInteger(day) && day >= 1 && day <= 7) ? "weekday" : "number";
    }
    if (!sample.every((item) => typeof item === "string")) return "";
    return sample.length && sample.every((item) => TIME.test(item)) ? "time" : "text";
  }

  const sortedType = (type) => type === "time" || type === "weekday";
  const clockKey = (text) => text.split(":").map((part) => part.padStart(2, "0")).join(":");
  const tidyList = (type, items) =>
    type === "time" ? [...items].sort((a, b) => clockKey(a).localeCompare(clockKey(b))) : type === "weekday" ? [...items].sort((a, b) => a - b) : items;

  function listEditor({ id, head, title, items, type, mode, path = "", name = "", describedby = "", plugin = "" }) {
    const value = h`<input class="tags-value" id="${id}" type="text" hidden tabindex="-1" autocomplete="off" value="${JSON.stringify(items)}"
        ${name ? h`name="${name}"` : ""} ${path ? h`data-path="${path}" data-kind="list"` : ""}>`;
    const shell = (inner) => h`<div class="field tags" role="group" aria-labelledby="${id}-label"${describedby ? h` aria-describedby="${describedby}"` : ""}
        data-tags="${mode}" data-type="${type}" data-title="${title}">${inner}${value}<span class="visually-hidden tags-live" role="status"></span></div>`;
    if (type === "image") return photoEditor({ id, head, title, items, mode, path, name, describedby, plugin });
    if (type === "weekday") {
      return shell(h`${head}<div class="chips weekdays">${WEEKDAYS.map((label, index) =>
        h`<button class="chip state" type="button" data-weekday="${index + 1}" aria-pressed="${bool(items.includes(index + 1))}">${icon("check")}周${label}</button>`)}</div>`);
    }
    const words = LIST_TEXT[type];
    const seconds = type === "time" && items.some((item) => item.split(":").length === 3);
    return shell(h`<div class="tags-head">${head}<span class="picker-count num">${items.length ? `${items.length} 项` : ""}</span></div>
      <ul class="tags-list" aria-label="${title} 的各项">${items.map(tagChip(type, items.length))}</ul>
      <div class="tags-add">
        <input class="input${type === "text" ? " mono" : " num"}" id="${id}-add" type="${type === "time" ? "time" : "text"}"
          ${type === "number" ? raw('inputmode="decimal"') : ""}${attr(seconds, 'step="1"')} enterkeyhint="done" spellcheck="false" autocapitalize="off" autocomplete="off"
          placeholder="${words.placeholder}" aria-label="添加到 ${title}" aria-describedby="${id}-hint">
        <button class="icon-btn state tags-cancel" type="button" data-tags-cancel aria-label="取消修改" hidden>${icon("close")}</button>
        <button class="btn btn-tonal tags-commit" type="button" data-tags-commit disabled>${icon("add")}<span>添加</span></button>
      </div>
      <span class="field-hint tags-hint" id="${id}-hint">${words.hint}</span>`);
  }

  /** 配图。相册里选的图先收进插件的本机图库（服务端 `data/<插件>/`），列表里只留文件名；
   *  也还能贴图片直链。一格方图一张：点开看原图，右上角 ✕ 是独立的移除键。 */
  function photoEditor({ id, head, title, items, mode, path = "", name = "", describedby = "", plugin = "" }) {
    const value = h`<input class="tags-value" id="${id}" type="text" hidden tabindex="-1" autocomplete="off" value="${JSON.stringify(items)}"
        ${name ? h`name="${name}"` : ""} ${path ? h`data-path="${path}" data-kind="list"` : ""}>`;
    return h`<div class="field tags photos" role="group" aria-labelledby="${id}-label"${describedby ? h` aria-describedby="${describedby}"` : ""}
        data-tags="${mode}" data-type="image" data-title="${title}" data-plugin="${plugin}">
      <div class="tags-head">${head}<span class="picker-count num">${items.length ? `${items.length} 张` : ""}</span></div>
      <ul class="photos-list" aria-label="${title} 的图片">${items.map(photoTile(plugin, items.length))}</ul>
      <div class="photos-empty"${attr(items.length, "hidden")}>
        <span class="photos-empty-icon" aria-hidden="true">${icon("perm_media")}</span>
        <span>还没有配图。</span>
        <span class="field-hint">从相册选一张，或者粘贴图片链接。</span>
      </div>
      <div class="photos-actions">
        <button class="btn btn-tonal" type="button" data-photo-pick>${icon("perm_media")}<span>从相册选择</span></button>
        <button class="btn btn-outlined" type="button" data-photo-link>${icon("add")}<span>粘贴链接</span></button>
        <input class="photo-file" type="file" accept="image/*" multiple hidden aria-hidden="true" tabindex="-1">
      </div>
      <div class="photos-link" hidden>
        <div class="tags-add">
          <input class="input mono" id="${id}-add" type="text" enterkeyhint="done" spellcheck="false" autocapitalize="off" autocomplete="off"
            placeholder="https://… 图片直链" aria-label="添加图片链接" aria-describedby="${id}-hint">
          <button class="btn btn-tonal tags-commit" type="button" data-tags-commit disabled>${icon("add")}<span>添加</span></button>
        </div>
        <span class="field-hint" id="${id}-hint">图片收在本机 data/${plugin}/ 里，配置里只存文件名；可以一次传多张。</span>
      </div>
      ${value}<span class="visually-hidden tags-live" role="status"></span>
    </div>`;
  }

  /** 一张配图的地址：直链原样用，其余当成本机图库里的文件名；给了宽度就要缩略图。 */
  function photoSrc(plugin, item, width) {
    const text = String(item).trim();
    if (/^https?:\/\//i.test(text)) return text;
    if (!plugin || !text || text.startsWith("/") || text.startsWith("~") || text.includes("\\")) return "";
    const query = `?t=${encodeURIComponent(token)}${width ? `&w=${width}` : ""}`;
    return `/api/plugins/${encodeURIComponent(plugin)}/asset/${encodeURIComponent(text)}${query}`;
  }

  /** 图下面那一行小字：直链说域名与文件名，本机文件说文件名，绝对路径说最后一段。 */
  function photoName(item) {
    const text = String(item).trim();
    if (/^https?:\/\//i.test(text)) {
      try {
        const url = new URL(text);
        const last = url.pathname.split("/").filter(Boolean).pop();
        return `${url.hostname}${last ? ` · ${last}` : ""}`;
      } catch {
        return text;
      }
    }
    return text.split(/[\\/]/).pop() || text;
  }

  /** 一格方图。图没加载出来时下面那枚图标留着当占位，不留一张破图。 */
  const photoTile = (plugin, total) => (item, index) => {
    const text = String(item);
    const src = photoSrc(plugin, text, 480);
    return h`<li class="photo" data-index="${index}">
      <button class="photo-open" type="button" data-photo-open="${index}" aria-label="查看第 ${index + 1} 张配图，共 ${total} 张">
        <span class="photos-void" aria-hidden="true">${icon("image")}</span>${src ? h`<img src="${src}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer">` : ""}
      </button>
      <button class="photo-remove" type="button" data-tags-remove="${index}" aria-label="移除第 ${index + 1} 张配图">${icon("close")}</button>
      <span class="photo-name" title="${text}">${photoName(text)}</span>
    </li>`;
  };

  /** 点一格方图看原图。原生 dialog 管 Esc 与焦点，关掉后焦点回到那一格。 */
  function photoPreview(root, index) {
    const items = readList(root);
    const item = items[index];
    if (item === undefined) return;
    const dialog = $("#dialog");
    const opener = document.activeElement;
    const src = photoSrc(root.dataset.plugin || "", item, 0);
    put(
      dialog,
      h`<h2 class="dialog-title" id="dialog-title">配图 ${index + 1} / ${items.length}</h2>
        <div class="dialog-photo">${src
          ? h`<img src="${src}" alt="第 ${index + 1} 张配图的预览" decoding="async">`
          : h`<span>这张是本机路径，页面读不到：<code>${String(item)}</code></span>`}</div>
        <p class="dialog-body" id="dialog-body">${String(item)}</p>
        <div class="actions actions-end"><button class="btn btn-filled" type="button" value="close">关闭</button></div>`
    );
    const close = (event) => {
      if (event.target.closest("button[value]")) dialog.close("close");
    };
    dialog.addEventListener("click", close);
    dialog.addEventListener(
      "close",
      () => {
        dialog.removeEventListener("click", close);
        if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
      },
      { once: true }
    );
    dialog.returnValue = "";
    dialog.showModal();
  }

  /** 一张图 POST 上去：字节走请求体，文件名放查询串（请求头只放得下 ASCII）。 */
  async function uploadPhoto(plugin, file) {
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), 60000);
    let response;
    try {
      response = await fetch(`/api/plugins/${encodeURIComponent(plugin)}/asset?name=${encodeURIComponent(file.name || "image")}`, {
        method: "POST",
        headers: { "x-acumen-token": token, "content-type": file.type || "image/*" },
        body: file,
        signal: abort.signal,
      });
    } catch {
      throw failure(abort.signal.aborted ? "这张图 60 秒没传完；换一张小一点的再试。" : "连不上控制台，图没有传上去。");
    } finally {
      clearTimeout(timer);
    }
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw failure(payload.error || `上传失败（${response.status}）`, response.status);
    return payload.name;
  }

  /** 一次传完选中的所有图，再一次性写进列表：中途失败就只加已经传上去的那几张。 */
  async function pickPhotos(root, files) {
    const plugin = root.dataset.plugin;
    if (!plugin) return;
    // 有的文件管理器不给 MIME：那就照收，服务端按字节判是不是图。
    const list = files.filter((file) => !file.type || file.type.startsWith("image/") || /\.(png|jpe?g|webp|gif)$/i.test(file.name));
    if (!list.length) return snackbar("只认得图片文件。", "error");
    const button = $("[data-photo-pick]", root);
    const label = $("span", button);
    const live = $(".tags-live", root);
    const said = label.textContent;
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    // 一张失败不耽误其余的：已经传上去的照样加进列表，失败的那几张在提示里点名。
    const added = [];
    let trouble = "";
    for (const [index, file] of list.entries()) {
      if (list.length > 1) label.textContent = `正在上传 ${index + 1}/${list.length}…`;
      live.textContent = `正在上传第 ${index + 1} 张，共 ${list.length} 张。`;
      try {
        added.push(await uploadPhoto(plugin, file));
      } catch (error) {
        trouble = error.message || String(error);
        if (error.status === 401) break;
      }
    }
    button.disabled = false;
    button.removeAttribute("aria-busy");
    label.textContent = said;
    live.textContent = "";
    if (!added.length) return trouble ? snackbar(trouble, "error") : undefined;
    const items = readList(root);
    const fresh = added.filter((name) => !items.includes(name));
    const dup = added.length - fresh.length;
    if (!fresh.length) return trouble ? snackbar(trouble, "error") : snackbar("这几张已经在列表里了。");
    void listEdit(root, [...items, ...fresh], `已加入 ${fresh.length} 张配图${dup ? `（${dup} 张重复，没再加）` : ""}${trouble ? `；有一张没传上去：${trouble}` : ""}`, {
      focus: () => $$("[data-photo-open]", root)[items.length]?.focus({ preventScroll: true }),
    });
  }

  /** 一枚芯片：本体是「修改」按钮，尾部 ✕ 是独立的命中区。 */
  const tagChip = (type, total) => (item, index) =>
    h`<li class="tag-chip" data-index="${index}">
      <button class="tag-chip-body" type="button" data-tags-edit="${index}"${sortedType(type) ? "" : raw(' aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight Delete"')}
        aria-label="${String(item)}，第 ${index + 1} 项，共 ${total} 项。点按修改">${String(item)}</button>
      <button class="tag-chip-remove" type="button" data-tags-remove="${index}" aria-label="移除 ${String(item)}">${icon("close")}</button>
    </li>`;

  const tagsValue = (root) => $(".tags-value", root);
  const tagsInput = (root) => $(".tags-add input", root);

  function readList(root) {
    try {
      const value = JSON.parse(tagsValue(root).value);
      return Array.isArray(value) ? value : [];
    } catch {
      return [];
    }
  }

  function paintTags(root) {
    const items = readList(root);
    const type = root.dataset.type;
    if (type === "image") {
      put($(".photos-list", root), items.map(photoTile(root.dataset.plugin || "", items.length)));
      $(".picker-count", root).textContent = items.length ? `${items.length} 张` : "";
      $(".photos-empty", root).hidden = items.length > 0;
      return;
    }
    if (type === "weekday") {
      for (const chip of $$("[data-weekday]", root)) chip.setAttribute("aria-pressed", bool(items.includes(Number(chip.dataset.weekday))));
      return;
    }
    put($(".tags-list", root), items.map(tagChip(type, items.length)));
    $(".picker-count", root).textContent = items.length ? `${items.length} 项` : "";
    const editing = root.dataset.editing;
    if (editing !== undefined) $(`.tag-chip[data-index="${editing}"]`, root)?.setAttribute("data-editing", "");
  }

  function syncTagsCommit(root) {
    const button = $("[data-tags-commit]", root);
    if (button) button.disabled = !tagsInput(root).value.trim();
  }

  /** 改列表的唯一入口：先改页面，再按模式写入；每次改动都能撤销（撤销本身不再给撤销）。 */
  async function listEdit(root, next, said, { undoable = true, focus = null } = {}) {
    const prev = readList(root);
    next = tidyList(root.dataset.type, next);
    if (same(prev, next)) return;
    const input = tagsValue(root);
    input.value = JSON.stringify(next);
    typed.add(input);
    paintTags(root);
    focus?.();
    const undo = undoable
      ? { label: "撤销", run: () => void listEdit(root, prev, "已撤销", { undoable: false, focus: () => tagsInput(root)?.focus({ preventScroll: true }) }) }
      : null;
    if (root.dataset.tags === "save") return saveList(root, input, tagsInput(root) ?? $("[data-weekday]", root), () => paintTags(root), said, undo);
    // emit：值交给外面的人去存（自动回复的规则随整条一起存）。
    if (root.dataset.tags === "emit") return void root.dispatchEvent(new CustomEvent("listchange", { bubbles: true, detail: { said, undo } }));
    syncGlobalDraft(root.closest("#global-form"));
    $(".tags-live", root).textContent = `${said}，现在共 ${next.length} 项。`;
    if (next.length < prev.length || !undoable) snackbar(`${said}。`, "info", undo);
  }

  const quote = (item) => `「${String(item).length > 20 ? `${String(item).slice(0, 20)}…` : item}」`;

  /** 输入框里的文字变成一项或几项。正在修改某一项时，是替换那一项。返回框里是否已经清空。 */
  function tagsSubmit(root, pasted = null) {
    const input = tagsInput(root);
    if (!input) return true;
    const type = root.dataset.type;
    // 粘贴的整段直接拿来拆：单行输入框会把换行吞掉。
    const text = (pasted ?? input.value).trim();
    const items = readList(root);
    const editing = root.dataset.editing === undefined ? -1 : Number(root.dataset.editing);
    if (!text) {
      if (editing >= 0) {
        // 改成空就是删掉这一项。
        stopEditing(root);
        void listEdit(root, items.filter((_, index) => index !== editing), `已移除${quote(items[editing])}`, { focus: () => input.focus({ preventScroll: true }) });
      }
      return true;
    }
    let pieces;
    if (type === "number") {
      pieces = text.normalize("NFKC").split(/[\s,，、;；]+/u).filter(Boolean).map((piece) => ({ piece, value: Number(piece) }));
      const bad = pieces.find(({ value }) => !Number.isFinite(value));
      if (bad) {
        fieldError(input, `「${bad.piece}」不是数字。`);
        return false;
      }
      pieces = pieces.map(({ value }) => value);
    } else if (type === "time") {
      if (!TIME.test(text)) {
        fieldError(input, "请选一个时间点，例如 08:30。");
        return false;
      }
      pieces = [text];
    } else pieces = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
    if (editing >= 0) {
      const value = pieces[0];
      const clash = items.findIndex((item, index) => index !== editing && same(item, value));
      if (clash >= 0) {
        flashChip(root, clash);
        fieldError(input, `${quote(value)} 已经在列表里了（第 ${clash + 1} 项）。`);
        return false;
      }
      stopEditing(root);
      input.value = "";
      syncTagsCommit(root);
      clearFieldError(input);
      if (!same(items[editing], value)) {
        const next = items.map((item, index) => (index === editing ? value : item));
        void listEdit(root, next, `已把${quote(items[editing])}改成${quote(value)}`, { focus: () => focusChip(root, tidyList(type, next).findIndex((item) => same(item, value))) });
      } else focusChip(root, editing);
      return true;
    }
    const fresh = [];
    for (const piece of pieces) if (!items.some((item) => same(item, piece)) && !fresh.some((item) => same(item, piece))) fresh.push(piece);
    input.value = "";
    syncTagsCommit(root);
    clearFieldError(input);
    if (!fresh.length) {
      const clash = items.findIndex((item) => same(item, pieces[0]));
      flashChip(root, clash);
      snackbar(pieces.length === 1 ? `${quote(pieces[0])} 已经在列表里了。` : "这些都已经在列表里了。");
      return true;
    }
    void listEdit(root, [...items, ...fresh], fresh.length === 1 ? `已添加${quote(fresh[0])}` : `已添加 ${fresh.length} 项`);
    return true;
  }

  function focusChip(root, index) {
    const chips = $$("[data-tags-edit]", root);
    (index >= 0 && chips.length ? chips[Math.min(index, chips.length - 1)] : tagsInput(root))?.focus({ preventScroll: true });
  }

  function flashChip(root, index) {
    const chip = $(`.tag-chip[data-index="${index}"]`, root);
    if (!chip) return;
    chip.removeAttribute("data-flash");
    void chip.offsetWidth;
    chip.setAttribute("data-flash", "");
  }

  /** 点一枚芯片：它的值进到下面的输入框里改，回车保存、Esc 取消；芯片标出「正在改」。 */
  function startEditing(root, index) {
    const items = readList(root);
    if (index < 0 || index >= items.length) return;
    const input = tagsInput(root);
    root.dataset.editing = String(index);
    for (const chip of $$(".tag-chip", root)) chip.toggleAttribute("data-editing", chip.dataset.index === String(index));
    input.value = String(items[index]);
    input.setAttribute("aria-label", `修改第 ${index + 1} 项`);
    $("[data-tags-cancel]", root).hidden = false;
    put($("[data-tags-commit]", root), h`${icon("check")}<span>保存</span>`);
    syncTagsCommit(root);
    clearFieldError(input);
    input.focus({ preventScroll: true });
    if (input.type === "text") input.select();
  }

  function stopEditing(root, { refocus = false } = {}) {
    const index = root.dataset.editing;
    if (index === undefined) return;
    delete root.dataset.editing;
    const input = tagsInput(root);
    input.setAttribute("aria-label", `添加到 ${root.dataset.title}`);
    $("[data-tags-cancel]", root).hidden = true;
    put($("[data-tags-commit]", root), h`${icon("add")}<span>添加</span>`);
    for (const chip of $$(".tag-chip[data-editing]", root)) chip.removeAttribute("data-editing");
    if (refocus) {
      input.value = "";
      syncTagsCommit(root);
      clearFieldError(input);
      focusChip(root, Number(index));
    }
  }

  function tagsRemove(button) {
    const root = button.closest(".tags");
    const index = Number(button.dataset.tagsRemove);
    const items = readList(root);
    if (root.dataset.editing !== undefined) stopEditing(root, { refocus: false });
    tagsInput(root).value = "";
    syncTagsCommit(root);
    void listEdit(root, items.filter((_, at) => at !== index), `已移除${quote(items[index])}`, {
      // 焦点交给顶上来的那一枚的 ✕：键盘用户连着删不必重新找位置。
      focus: () => {
        const removes = $$("[data-tags-remove]", root);
        (removes[Math.min(index, removes.length - 1)] ?? tagsInput(root))?.focus({ preventScroll: true });
      },
    });
  }

  function tagsMove(root, from, to) {
    const items = readList(root);
    if (to < 0 || to >= items.length || from === to || sortedType(root.dataset.type)) return;
    const next = [...items];
    const [item] = next.splice(from, 1);
    next.splice(to, 0, item);
    void listEdit(root, next, `已把${quote(item)}移到第 ${to + 1} 项`, { focus: () => focusChip(root, to) });
  }

  function toggleWeekday(button) {
    const root = button.closest(".tags");
    const day = Number(button.dataset.weekday);
    const items = readList(root);
    const on = items.includes(day);
    void listEdit(root, on ? items.filter((item) => item !== day) : [...items, day], `已${on ? "去掉" : "加上"}周${WEEKDAYS[day - 1]}`, { undoable: on });
  }

  /** 拖动排序。鼠标按下移动几像素就开始；触屏要先按住一小会儿（否则就是滚动页面）。
   *  拖的时候芯片跟着手走，经过别的芯片就换位，松手才写入。 */
  function bindTagDrag() {
    let drag = null;
    const finish = (commit) => {
      if (!drag) return;
      const { root, chip, from, timer, active } = drag;
      clearTimeout(timer);
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
      removeEventListener("pointercancel", cancel);
      drag = null;
      if (!active) return;
      chip.removeAttribute("data-dragging");
      chip.style.translate = "";
      root.removeAttribute("data-dragging");
      dragSuppress = Date.now() + 400;
      const to = $$(".tag-chip", root).indexOf(chip);
      if (commit && to !== from) tagsMove(root, from, to);
      else paintTags(root);
    };
    const place = (event) => {
      const { root, chip } = drag;
      const others = $$(".tag-chip", root).filter((node) => node !== chip);
      let target = null;
      for (const node of others) {
        const box = node.getBoundingClientRect();
        if (event.clientY >= box.top && event.clientY <= box.bottom && event.clientX >= box.left && event.clientX <= box.right) target = node;
      }
      if (target) {
        const box = target.getBoundingClientRect();
        const after = event.clientX > box.left + box.width / 2;
        const anchor = after ? target.nextElementSibling : target;
        if (anchor !== chip && anchor !== chip.nextElementSibling) {
          target.parentNode.insertBefore(chip, anchor);
          if (navigator.vibrate) navigator.vibrate(5);
        }
      }
      // 芯片在 DOM 里换了位，位移要按它的新位置重算，才一直贴着手指。
      chip.style.translate = "";
      const box = chip.getBoundingClientRect();
      chip.style.translate = `${event.clientX - drag.dx - box.left}px ${event.clientY - drag.dy - box.top}px`;
    };
    const activate = () => {
      if (!drag || drag.active) return;
      drag.active = true;
      const box = drag.chip.getBoundingClientRect();
      drag.dx = drag.x0 - box.left;
      drag.dy = drag.y0 - box.top;
      drag.chip.setAttribute("data-dragging", "");
      drag.root.setAttribute("data-dragging", "");
      if (navigator.vibrate) navigator.vibrate(8);
    };
    const move = (event) => {
      if (!drag || event.pointerId !== drag.id) return;
      const far = Math.hypot(event.clientX - drag.x0, event.clientY - drag.y0);
      if (!drag.active) {
        if (drag.touch) {
          if (far > 8) finish(false); // 按住之前就动了：那是在滚页面
          return;
        }
        if (far < 6) return;
        activate();
      }
      place(event);
    };
    const up = (event) => {
      if (drag && event.pointerId === drag.id) finish(true);
    };
    const cancel = () => finish(false);
    document.addEventListener("pointerdown", (event) => {
      const body = event.target.closest?.(".tag-chip-body");
      const root = body?.closest(".tags");
      if (!body || !root || event.button !== 0 || sortedType(root.dataset.type) || readList(root).length < 2 || root.dataset.editing !== undefined) return;
      const chip = body.closest(".tag-chip");
      drag = { root, chip, id: event.pointerId, x0: event.clientX, y0: event.clientY, from: $$(".tag-chip", root).indexOf(chip), touch: event.pointerType !== "mouse", active: false };
      if (drag.touch) drag.timer = setTimeout(activate, 320);
      addEventListener("pointermove", move);
      addEventListener("pointerup", up);
      addEventListener("pointercancel", cancel);
    });
    // 已经拖起来了就不让页面跟着滚；没拖起来照常滚动。
    document.addEventListener("touchmove", (event) => {
      if (drag?.active && event.cancelable) event.preventDefault();
    }, { passive: false });
    // 按住时浏览器会弹长按菜单，拖动里不需要它。
    document.addEventListener("contextmenu", (event) => {
      if (drag && event.target.closest?.(".tag-chip-body")) event.preventDefault();
    });
  }
  let dragSuppress = 0;

  /** 把列表写进插件配置（名单编辑器与列表编辑器共用）。保存期间的新改动合并成一次补写，
   *  失败则退回上次保存的样子，原因写在 `anchor`（添加框）底下。 */
  async function saveList(root, input, anchor, repaint, said, undo) {
    const form = input.closest("[data-config]");
    if (!form) return;
    if (root.dataset.saving) {
      root.pending = { said, undo };
      return;
    }
    const submitted = input.value;
    if (submitted === input.defaultValue) return;
    root.dataset.saving = "1";
    input.setAttribute("aria-busy", "true");
    try {
      const reply = await configPost(form, "config", { path: input.dataset.path, value: JSON.parse(submitted) });
      input.defaultValue = submitted;
      if (anchor?.id) clearFieldError(anchor);
      markModified(root.closest("[data-setting]"), JSON.parse(submitted));
      snackbar(`${said}，已保存。${reply.message.replace(/^已保存 [^。]*。/, "")}`, "info", undo);
      refreshDiff(form.dataset.config);
    } catch (error) {
      if (error.status === 401) return;
      input.value = input.defaultValue;
      repaint();
      // 锚点在收起来的「粘贴链接」里时，错误写在看不见的地方等于没说：改成提示条。
      if (anchor?.id && !anchor.closest("[hidden]")) fieldError(anchor, `没能保存，${root.matches(".picker") ? "名单" : "列表"}已退回原样：${error.message}`);
      else report(error);
    } finally {
      delete root.dataset.saving;
      input.removeAttribute("aria-busy");
      const later = root.pending;
      root.pending = null;
      if (later && root.isConnected && input.value !== input.defaultValue) void saveList(root, input, anchor, repaint, later.said, later.undo);
    }
  }


  /* ==================== §11c 外观 ====================
     主题色、配色方案、明暗、对比度与字号。改了立刻生效，不设「保存」：这是偏好，不是配置。
     - 配色由 /theme.js（Material Color Utilities，与 make-tokens.py 同一套 2025 规范）在页面里算，
       算好的 CSS 连同选择一起记在 localStorage，index.html 的小脚本首帧就贴回去；
     - 四个选择同步到 /api/theme，换一台设备打开也是这个样子；字号只属于这台设备；
     - 全是默认值时什么都不注入，界面用的就是 tokens.css 里那份生成的令牌。 */

  const THEME_KEY = "acumen.theme";
  /* 引擎的取色表改过就加一：旧版算好的缓存不再贴（index.html 首帧那段认同一个数），登录后按原来的选择重算。 */
  const THEME_ENGINE = 3;
  const THEME_DEFAULT = { seed: "#3f51b5", variant: "fidelity", mode: "system", contrast: "system" };
  const SEEDS = [
    ["#3f51b5", "靛青"], ["#0061a4", "海蓝"], ["#006a6a", "青碧"], ["#386a20", "松绿"],
    ["#8b5000", "琥珀"], ["#b3261e", "朱砂"], ["#b0275b", "海棠"], ["#6750a4", "紫藤"], ["#5f6368", "石墨"],
  ];
  const VARIANTS = [
    ["fidelity", "原色", "默认。主色贴近你选的那一个，表面仍保持柔和"],
    ["tonal", "柔和", "主色收一点、表面带一丝色调，久看不累眼"],
    ["vibrant", "鲜明", "彩度拉满，按钮与选中更醒目"],
    ["expressive", "跳色", "主色与第三色拉开色相，更有个性"],
    ["neutral", "素雅", "几乎只剩灰，颜色只留给要紧的地方"],
    ["monochrome", "黑白", "纯灰阶，只靠明暗分层"],
  ];
  const MODES = [["system", "跟随系统", "brightness_auto"], ["light", "浅色", "light_mode"], ["dark", "深色", "dark_mode"]];
  const CONTRASTS = [["system", "跟随系统"], ["standard", "标准"], ["medium", "中"], ["high", "高"]];
  const SCALES = [[0.9375, "小"], [1, "标准"], [1.125, "大"], [1.25, "特大"]];

  const appearance = {
    engine: null,
    picked: [],
    syncTimer: 0,
    /** 改动排成一队：连着点两项时，后一项要在前一项写完之后再读，不然会把前一项盖掉。 */
    queue: Promise.resolve(),

    stored() {
      try {
        return JSON.parse(localStorage.getItem(THEME_KEY) || "null") || {};
      } catch {
        return {};
      }
    },
    prefs() {
      return { ...THEME_DEFAULT, ...(this.stored().prefs || {}) };
    },
    scale() {
      return this.stored().scale || 1;
    },

    /** 配色引擎按需加载一次。 */
    load() {
      if (this.engine) return Promise.resolve(this.engine);
      if (window.ZwTheme) return Promise.resolve((this.engine = window.ZwTheme));
      return new Promise((resolve, reject) => {
        const script = document.createElement("script");
        script.src = "/theme.js";
        script.onload = () => resolve((this.engine = window.ZwTheme));
        script.onerror = () => reject(failure("配色引擎没能加载，刷新页面再试。"));
        document.head.append(script);
      });
    },

    isDefault: (prefs) => Object.keys(THEME_DEFAULT).every((key) => prefs[key] === THEME_DEFAULT[key]),

    /** 算好、记下、贴上。`sync` 为真时顺手排一次同步。 */
    set(change, options) {
      const run = this.queue.then(() => this.commit(change, options));
      this.queue = run.catch(() => {});
      return run;
    },

    async commit(change, { sync = true } = {}) {
      const stored = this.stored();
      const prefs = { ...THEME_DEFAULT, ...(stored.prefs || {}), ...change };
      let css = "";
      let surface = null;
      if (!this.isDefault(prefs)) {
        const engine = await this.load();
        const built = engine.build(prefs);
        css = built.css;
        surface = built.surface;
      }
      const next = { ...stored, prefs, css, surface, engine: THEME_ENGINE };
      this.write(next);
      this.apply(next);
      if (sync) this.push(prefs);
    },

    setScale(scale) {
      this.queue = this.queue.then(() => {
        const next = { ...this.stored(), scale };
        this.write(next);
        this.apply(next);
      });
      return this.queue;
    },

    write(value) {
      try {
        localStorage.setItem(THEME_KEY, JSON.stringify(value));
      } catch {
        // 无痕模式之类存不下：这一次照样生效，只是下次打开回到同步的那一份。
      }
    },

    /** 与 index.html 首帧那段同一个效果；这里还负责撤掉旧的。 */
    apply({ prefs = THEME_DEFAULT, css = "", surface = null, scale = 1 }) {
      let sheet = $("#zw-theme");
      if (css) {
        if (!sheet) {
          sheet = document.createElement("style");
          sheet.id = "zw-theme";
          document.head.append(sheet);
        }
        sheet.textContent = css;
      } else sheet?.remove();
      document.documentElement.style.fontSize = scale && scale !== 1 ? `${scale * 100}%` : "";
      const forced = prefs.mode === "light" || prefs.mode === "dark" ? prefs.mode : null;
      const scheme = $('meta[name="color-scheme"]');
      if (scheme) scheme.content = forced || "light dark";
      for (const meta of $$('meta[name="theme-color"]')) {
        meta.dataset.base ??= meta.content;
        const dark = forced === "dark" || (!forced && /dark/.test(meta.media));
        meta.content = surface ? (dark ? surface.dark : surface.light) : meta.dataset.base;
      }
      // 导航胶囊的位置与配色无关，但字号变了格子也变了。
      requestAnimationFrame(() => syncNavPill());
    },

    /** 连着改（拖着取色器）只同步最后一次。失败才出声：成功本来就是看得见的。 */
    push(prefs) {
      clearTimeout(this.syncTimer);
      this.syncTimer = setTimeout(async () => {
        try {
          await api("/theme", { method: "POST", body: prefs });
        } catch (error) {
          if (error.status === 401 || error.offline) return;
          snackbar(`外观只在这台设备上生效：${error.message}`, "error");
        }
      }, 600);
    },

    /** 登录后对一次：别的设备改过主题，这里跟上。 */
    async pull() {
      let remote;
      try {
        remote = await api("/theme");
      } catch {
        return this.refresh();
      }
      const local = this.prefs();
      const wanted = remote ? { ...THEME_DEFAULT, ...remote } : THEME_DEFAULT;
      if (Object.keys(THEME_DEFAULT).every((key) => wanted[key] === local[key])) return this.refresh();
      // 本机改过而服务端从没存过：以本机为准，补存一份。
      if (!remote && !this.isDefault(local)) {
        this.push(local);
        return this.refresh();
      }
      await this.set(wanted, { sync: false }).catch(() => {});
      if (shown?.key === "settings/appearance") render({ quiet: true });
    },

    /** 缓存是旧版引擎算的：选择不变，按现在的取色表重算一遍。 */
    refresh() {
      const stored = this.stored();
      if (!stored.prefs || stored.engine === THEME_ENGINE) return;
      return this.set({}, { sync: false }).catch(() => {});
    },

    summary() {
      const prefs = this.prefs();
      const seed = SEEDS.find(([hex]) => hex === prefs.seed)?.[1] || prefs.seed.toUpperCase();
      const variant = VARIANTS.find(([id]) => id === prefs.variant)?.[1];
      const mode = MODES.find(([id]) => id === prefs.mode)?.[1];
      const scale = SCALES.find(([value]) => value === this.scale())?.[1];
      return `${seed} · ${variant} · ${mode === "跟随系统" ? "明暗跟随系统" : mode} · 字号${scale || "自定"}`;
    },

    mount() {
      const root = $("#appearance");
      if (!root) return;
      root.addEventListener("change", (event) => this.onChange(event));
      root.addEventListener("input", (event) => this.onInput(event));
      root.addEventListener("click", (event) => this.onClick(event));
    },

    onInput(event) {
      const target = event.target;
      // 拖着取色器时逐帧跟随；松手的 change 才同步。
      if (target.name === "seed-custom") {
        cancelAnimationFrame(this.frame);
        this.frame = requestAnimationFrame(() => this.pickSeed(target.value, { sync: false, live: true }));
      }
    },

    async onChange(event) {
      const target = event.target;
      try {
        if (target.name === "seed" || target.name === "seed-custom") await this.pickSeed(target.value);
        else if (target.name === "variant") await this.set({ variant: target.value });
        else if (target.name === "mode") await this.set({ mode: target.value });
        else if (target.name === "contrast") await this.set({ contrast: target.value });
        else if (target.name === "scale") await this.setScale(Number(target.value));
        else if (target.name === "seed-image") await this.fromImage(target);
        else return;
      } catch (error) {
        snackbar(error.message, "error");
        return;
      }
      $("#appearance-summary")?.replaceChildren(this.summary());
    },

    async onClick(event) {
      if (!event.target.closest("[data-theme-reset]")) return;
      this.picked = [];
      this.setScale(1);
      await this.set({ ...THEME_DEFAULT });
      await render({ quiet: true });
      snackbar("外观已恢复默认。");
    },

    /** 换种子色：色板的小样要跟着方案重画，其余控件不动（焦点与滚动都留着）。 */
    async pickSeed(value, { sync = true, live = false } = {}) {
      const seed = String(value).toLowerCase();
      if (!/^#[0-9a-f]{6}$/.test(seed)) return;
      await this.set({ seed }, { sync });
      const custom = $("#seed-custom");
      if (!live && custom && custom.value !== seed) custom.value = seed;
      const known = $$('#appearance input[name="seed"]').some((radio) => radio.value === seed);
      for (const radio of $$('#appearance input[name="seed"]')) radio.checked = radio.value === seed;
      $("#seed-custom-swatch")?.toggleAttribute("data-on", !known);
      const hex = $("#seed-custom-hex");
      if (hex) hex.textContent = known ? "" : seed.toUpperCase();
      this.paintVariants(seed);
    },

    paintVariants(seed) {
      if (!this.engine) return;
      for (const card of $$("#appearance [data-variant-sample]")) {
        const sample = this.engine.sample(seed, card.dataset.variantSample);
        card.style.setProperty("--_a", sample.primary);
        card.style.setProperty("--_b", sample.secondary);
        card.style.setProperty("--_c", sample.tertiary);
      }
    },

    /** 从一张图里取几枚候选：缩到 128px 再量化，大图也只算一万多个像素。 */
    async fromImage(input) {
      const file = input.files?.[0];
      input.value = "";
      if (!file) return;
      const engine = await this.load();
      let bitmap;
      try {
        bitmap = await createImageBitmap(file);
      } catch {
        throw failure("这张图打不开，换一张试试。");
      }
      const side = 128;
      const ratio = Math.min(1, side / Math.max(bitmap.width, bitmap.height));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(bitmap.width * ratio));
      canvas.height = Math.max(1, Math.round(bitmap.height * ratio));
      const context = canvas.getContext("2d", { willReadFrequently: true });
      context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      bitmap.close?.();
      const colors = engine.fromImage(context.getImageData(0, 0, canvas.width, canvas.height).data, 4);
      if (!colors.length) throw failure("这张图里取不出像样的颜色，换一张色彩多一点的。");
      this.picked = colors;
      await this.pickSeed(colors[0]);
      await render({ quiet: true });
      $('#appearance input[name="seed"]:checked')?.focus();
      snackbar(`从图里取到 ${colors.length} 种颜色，已换上第一种。`);
    },
  };

  const swatch = (hex, label, checked, engine) => {
    const sample = engine.sample(hex, "tonal");
    return h`<label class="swatch state" style="--_seed:${hex};--_a:${sample.primary};--_b:${sample.secondary};--_c:${sample.tertiary}">
      <input type="radio" name="seed" value="${hex}"${attr(checked, "checked")}>
      <span class="swatch-disc" aria-hidden="true">${icon("check")}</span>
      <span class="swatch-label">${label}</span>
    </label>`;
  };

  async function appearancePage() {
    const engine = await appearance.load();
    const prefs = appearance.prefs();
    const scale = appearance.scale();
    const presets = SEEDS.map(([hex]) => hex);
    const picked = appearance.picked.filter((hex) => !presets.includes(hex));
    const custom = !presets.includes(prefs.seed) && !picked.includes(prefs.seed);
    const radio = (name, value, checked, label, glyph = "") =>
      h`<label class="segment"><input type="radio" name="${name}" value="${value}"${attr(checked, "checked")}>${glyph ? icon(glyph) : ""}${label}</label>`;
    return h`<div class="page" id="appearance" data-title="外观">
      ${pageHead("外观", "只改这个控制台的样子，群里的卡片不受影响。改了立刻生效。", {
        back: { href: "#/settings", label: "设置" },
      })}

      <section class="card look-preview" aria-labelledby="look-preview-title">
        <h2 class="visually-hidden" id="look-preview-title">预览</h2>
        <div class="look-hero" aria-hidden="true">
          <span class="look-hero-label">运行中</span>
          <span class="look-hero-value">3 天 4 小时</span>
          <span class="look-hero-dot"></span>
        </div>
        <div class="look-row" aria-hidden="true">
          <span class="tile">${icon("palette")}</span>
          <span class="look-lines"><b>预览</b><span>按钮、开关与状态都跟着变</span></span>
          <span class="switch look-switch" role="presentation" aria-checked="true"></span>
        </div>
        <div class="look-actions" aria-hidden="true">
          <span class="btn btn-filled">主要</span><span class="btn btn-tonal">次要</span><span class="chip" aria-pressed="true">${icon("check")}已选</span>
          <span class="status status-success">成功</span><span class="status status-warning">提醒</span>
        </div>
        <p class="note" id="appearance-summary">${appearance.summary()}</p>
      </section>

      <section class="section" aria-labelledby="seed-title">
        <div class="section-head">
          <h2 class="section-title" id="seed-title">主题色</h2>
          <span class="section-meta">整套配色由它推出，文字对比度自动达标</span>
        </div>
        <fieldset class="swatches" aria-labelledby="seed-title">
          ${SEEDS.map(([hex, label]) => swatch(hex, label, hex === prefs.seed, engine))}
          ${picked.map((hex, index) => swatch(hex, `图中 ${index + 1}`, hex === prefs.seed, engine))}
          <label class="swatch swatch-custom state" id="seed-custom-swatch" style="--_seed:${prefs.seed}"${attr(custom, "data-on")}>
            <input type="color" name="seed-custom" id="seed-custom" value="${prefs.seed}" aria-describedby="seed-custom-hex">
            <span class="swatch-disc" aria-hidden="true">${icon("colorize")}</span>
            <span class="swatch-label">自选 <span class="num" id="seed-custom-hex">${custom ? prefs.seed.toUpperCase() : ""}</span></span>
          </label>
        </fieldset>
        <div class="actions">
          <label class="btn btn-tonal state file-btn">${icon("image")}从图片取色
            <input type="file" name="seed-image" accept="image/*" class="visually-hidden"></label>
        </div>
      </section>

      <section class="section" aria-labelledby="variant-title">
        <div class="section-head"><h2 class="section-title" id="variant-title">配色风格</h2></div>
        <fieldset class="variants" aria-labelledby="variant-title">
          ${VARIANTS.map(([id, label, hint]) => {
            const sample = engine.sample(prefs.seed, id);
            return h`<label class="variant state" data-variant-sample="${id}" style="--_a:${sample.primary};--_b:${sample.secondary};--_c:${sample.tertiary}">
              <input type="radio" name="variant" value="${id}"${attr(id === prefs.variant, "checked")} aria-describedby="variant-${id}-hint">
              <span class="variant-strip" aria-hidden="true"><i></i><i></i><i></i></span>
              <span class="variant-text"><span class="variant-name">${label}</span><span class="variant-hint" id="variant-${id}-hint">${hint}</span></span>
              <span class="variant-check" aria-hidden="true">${icon("check_circle-fill")}</span>
            </label>`;
          })}
        </fieldset>
      </section>

      <section class="section" aria-labelledby="mode-title">
        <div class="config-group">
          <div class="setting">
            <span class="field-label" id="mode-title">明暗</span>
            <fieldset class="segmented segmented-fill" aria-labelledby="mode-title">
              ${MODES.map(([id, label, glyph]) => radio("mode", id, id === prefs.mode, label, glyph))}
            </fieldset>
          </div>
          <div class="setting">
            <span class="field-label" id="contrast-title">对比度</span>
            <span class="field-hint" id="contrast-hint">跟随系统时，打开系统的「增强对比度」就换成中档。</span>
            <fieldset class="segmented segmented-fill" aria-labelledby="contrast-title" aria-describedby="contrast-hint">
              ${CONTRASTS.map(([id, label]) => radio("contrast", id, id === prefs.contrast, label))}
            </fieldset>
          </div>
          <div class="setting">
            <span class="field-label" id="scale-title">字号</span>
            <span class="field-hint" id="scale-hint">只在这台设备上生效；整页按比例放大，排版不会挤。</span>
            <fieldset class="segmented segmented-fill" aria-labelledby="scale-title" aria-describedby="scale-hint">
              ${SCALES.map(([value, label]) => radio("scale", value, value === scale, label))}
            </fieldset>
          </div>
        </div>
      </section>

      <div class="actions">
        <button class="btn btn-text" type="button" data-theme-reset>${icon("restart_alt")}恢复默认外观</button>
      </div>
    </div>`;
  }

  /* ==================== §12 解锁 ==================== */

  function lock(reason = "") {
    token = "";
    store.clear();
    logs.stop();
    overviewFeed.stop();
    renderSeq++;
    shown = null;
    busy(false);
    document.body.dataset.state = "locked";
    setTitle("解锁");
    $("#topbar").removeAttribute("data-scrolled");
    put(
      $("#view"),
      h`<div class="lock"><form class="lock-card" id="lock-form">
        <span class="brand-logo lock-logo" aria-hidden="true"><span class="brand-mark"></span><span class="brand-glimmer"></span></span>
        <h1 class="lock-title page-title" tabindex="-1">解锁知微控制台</h1>
        <p class="note">${reason || "需要访问口令才能查看这台设备的数据。"}
          启动日志里带 <code>?t=</code> 的地址可以直接打开；口令也保存在 <code>data/console/token</code>。</p>
        <input type="text" name="username" autocomplete="username" value="acumen" hidden>
        <div class="field">
          <label class="field-label" for="lock-input">访问口令</label>
          <div class="reveal">
            <input class="input mono" id="lock-input" name="password" type="password" autocomplete="current-password"
              required spellcheck="false" autocapitalize="off">
            <button class="icon-btn state" type="button" data-reveal="#lock-input" aria-pressed="false" aria-label="显示口令">${icon("visibility")}</button>
          </div>
        </div>
        <button class="btn btn-filled" type="submit">解锁</button>
      </form></div>`
    );
    $("#lock-input").focus();
  }

  function unlock(value) {
    token = value;
    store.set(value);
    document.body.dataset.state = "ready";
    lifecycle.probe().finally(() => {
      render();
      updates.check();
      appearance.pull();
    });
  }

  /* ==================== §13 事件 ==================== */

  function bindEvents() {
    /* 头像加载失败（离线、CDN 拦了、链接过期）时退回首字，不留一张破图。 */
    addEventListener("error", (event) => {
      const img = event.target;
      if (!(img instanceof HTMLImageElement) || img.dataset.initial === undefined) return;
      img.closest(".tile-avatar, .lk-face")?.replaceChildren(img.dataset.initial || "?");
    }, true);

    /* 导航尺寸变了（断点、旋转、字体）就重新量一次胶囊；不参与换页动画。 */
    let navResize = 0;
    const repaintNav = () => {
      cancelAnimationFrame(navResize);
      navResize = requestAnimationFrame(syncNavPill);
    };
    addEventListener("resize", repaintNav, { passive: true });
    if ("ResizeObserver" in window) new ResizeObserver(repaintNav).observe($("#nav"));
    document.fonts?.ready.then(repaintNav);

    document.addEventListener("click", async (event) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const on = (selector) => target.closest(selector);
      let hit;

      if (on("[data-snack-action]")) {
        const run = snack.action;
        clearTimeout(snack.timer);
        put($("#snackbar"), "");
        run?.();
        return;
      }
      if (on("[data-snack-close]")) {
        clearTimeout(snack.timer);
        put($("#snackbar"), "");
        return;
      }
      if ((hit = on("[data-reveal]"))) {
        const field = $(hit.dataset.reveal);
        const show = field.type === "password";
        field.type = show ? "text" : "password";
        hit.setAttribute("aria-pressed", bool(show));
        hit.setAttribute("aria-label", show ? "隐藏口令" : "显示口令");
        put(hit, icon(show ? "visibility_off" : "visibility"));
        field.focus();
        return;
      }
      if ((hit = on("[data-photo-pick]"))) {
        $(".photo-file", hit.closest(".photos"))?.click();
        return;
      }
      if ((hit = on("[data-photo-link]"))) {
        const row = $(".photos-link", hit.closest(".photos"));
        row.hidden = false;
        $("input", row)?.focus({ preventScroll: true });
        return;
      }
      if ((hit = on("[data-photo-open]"))) return photoPreview(hit.closest(".photos"), Number(hit.dataset.photoOpen));
      if ((hit = on("[data-tags-edit]"))) {
        if (Date.now() < dragSuppress) return;
        const root = hit.closest(".tags");
        const index = Number(hit.dataset.tagsEdit);
        if (root.dataset.editing === String(index)) return stopEditing(root, { refocus: true });
        return startEditing(root, index);
      }
      if ((hit = on("[data-tags-remove]"))) return tagsRemove(hit);
      if ((hit = on("[data-tags-commit]"))) return void tagsSubmit(hit.closest(".tags"));
      if ((hit = on("[data-tags-cancel]"))) return stopEditing(hit.closest(".tags"), { refocus: true });
      if ((hit = on("[data-weekday]"))) return toggleWeekday(hit);
      if ((hit = on("[data-tpl-insert]"))) return tplInsert(hit);
      if ((hit = on("[data-tpl-image]"))) {
        $(".tpl-image-file", hit.closest("[data-tpl]"))?.click();
        return;
      }
      if (on("[data-rule-new]")) return ruleNew();
      if ((hit = on("[data-rules-enable]"))) return rulesEnable(hit);
      if ((hit = on("[data-rule-switch]"))) return ruleSwitch(hit);
      if ((hit = on("[data-rule-delete]"))) return ruleDelete(hit);
      if ((hit = on("[data-rule-restore]"))) return ruleRestore(Number(hit.dataset.ruleRestore));
      if ((hit = on("[data-rule-show]"))) return ruleShow(hit.dataset.ruleShow);
      if ((hit = on("[data-forward-remove]"))) return forwardRemove(hit);
      if ((hit = on("[data-tpl-add]"))) {
        const root = hit.closest("[data-tpl]");
        const variants = tplRead(root);
        const fresh = tplRedraw(root, [...variants, ""], variants.length);
        fresh.dataset.tplActive = String(variants.length);
        return;
      }
      if ((hit = on("[data-tpl-remove]"))) {
        const root = hit.closest("[data-tpl]");
        const index = Number(hit.dataset.tplRemove);
        const variants = tplRead(root);
        const [gone] = variants.splice(index, 1);
        const fresh = tplRedraw(root, variants, variants.length ? Math.min(index, variants.length - 1) : -1);
        await tplSave(fresh);
        if (gone.trim()) {
          snackbar(`删掉了说法 ${index + 1}`, "info", {
            label: "撤销",
            run: () => {
              const now = $(`[data-tpl="${CSS.escape(fresh.dataset.tpl)}"]`);
              if (!now) return;
              const back = tplRead(now);
              back.splice(index, 0, gone);
              void tplSave(tplRedraw(now, back, index));
            },
          });
        }
        return;
      }
      if ((hit = on("[data-reset-path]"))) return resetField(hit);
      if ((hit = on("[data-inherit]"))) return entryField(hit, true);
      if ((hit = on("[data-entry-field]"))) return entryField(hit, false);
      if ((hit = on("[data-entry-remove]"))) return removeEntry(hit);
      if ((hit = on("[data-map-add]"))) return addEntries(hit);
      if ((hit = on("[data-picker-remove]"))) return pickerRemove(hit);
      if ((hit = on("[data-picker-add]"))) return void pickerSubmitText(hit.closest(".picker"), { explicit: true });
      if ((hit = on("[data-picker-open]"))) return pickerChoose(hit.closest(".picker"));
      if (on("#refresh-page")) {
        const button = $("#refresh-page");
        if (button.dataset.busy !== undefined) return;
        button.dataset.busy = "";
        await render({ quiet: true }).finally(() => delete button.dataset.busy);
        return;
      }
      if (on("[data-retry]")) return render();
      // 设置行里点键名或说明，等于点它的开关：整行都是这个开关的命中区（HIG）。
      if ((hit = on(".toggle-row.setting")) && !on("button, a, input, select, textarea, label") && !getSelection()?.toString()) {
        const control = $(":scope > .switch", hit);
        if (control && !control.disabled) {
          control.focus({ preventScroll: true });
          return control.dataset.ruleSwitch !== undefined ? ruleSwitch(control) : commitField(control);
        }
      }
      if ((hit = on("[data-toggle]"))) return togglePlugin(hit);
      if ((hit = on('button[data-path][data-kind="bool"]'))) return commitField(hit);
      if ((hit = on("[data-sticker-play]"))) {
        const play = hit.getAttribute("aria-pressed") !== "true";
        hit.setAttribute("aria-pressed", String(play));
        hit.textContent = play ? "停止播放" : "播放原图";
        $("img", hit.closest("figure")).src = `/api/ambient/sticker/${hit.dataset.stickerPlay}?t=${encodeURIComponent(token)}${play ? "&play=true" : ""}`;
        return;
      }
      if ((hit = on("[data-form-switch]"))) {
        const checked = hit.getAttribute("aria-checked") !== "true";
        hit.setAttribute("aria-checked", String(checked));
        // 白名单开着就不查黑名单：开关一动，两张名单底下那句「现在的效果」都要跟着变。
        if (hit.name === "enable_whitelist" || hit.name === "enable_blacklist") refreshStatuses(hit.closest("form"));
        if (hit.name === "clear_token") {
          // 要清掉就别再填新的：令牌框停用并清空，免得两个意思同时提交。
          const field = tokenField(hit.closest("form"));
          if (field) {
            field.disabled = checked;
            if (checked) field.value = "";
            field.placeholder = checked ? "保存后改为不鉴权" : "已设置；留空保持不变";
          }
        }
        return;
      }
      if ((hit = on("[data-hint-more]"))) {
        const target = document.getElementById(hit.dataset.hintMore);
        const open = !target?.hasAttribute("data-open");
        target?.toggleAttribute("data-open", open);
        hit.setAttribute("aria-expanded", bool(open));
        hit.textContent = open ? "收起" : "展开";
        return;
      }
      if ((hit = on("[data-status-filter]"))) {
        plugins.status = hit.dataset.statusFilter;
        plugins.repaint();
        $("[data-status-filter]")?.focus({ preventScroll: true });
        return;
      }
      if ((hit = on("[data-section]"))) {
        plugins.chooseSection(hit.dataset.section);
        return;
      }
      if ((hit = on("[data-clear-filters]"))) {
        plugins.clearFilters();
        return;
      }
      if ((hit = on("[data-reset]"))) return resetPlugin(hit);
      if ((hit = on("[data-live-test]"))) return testLive(hit);
      if ((hit = on("[data-copy]"))) return copyText(hit.dataset.copy);
      if ((hit = on('[role="tab"]'))) return selectTab(hit);
      if ((hit = on("[data-save-source]"))) return saveSource(hit);
      if (on("[data-more-stickers]")) {
        ambient.stickers += STICKER_PAGE;
        const panel = $("#panel-stickers");
        const before = $$("#sticker-grid > li").length;
        put(panel, stickerPanel());
        // 焦点落到新出现的第一张上，键盘用户不必从头再翻。
        const first = $$("#sticker-grid > li")[before];
        first?.setAttribute("tabindex", "-1");
        first?.focus();
        return;
      }
      if (on("#log-follow")) return logs.setFollow(!logs.follow);
      if (on("#log-jump")) {
        logs.setFollow(true);
        logs.box()?.focus();
        return;
      }
      if (on("#log-export")) return logs.export();
      if (on("#log-clear")) {
        logs.lines = [];
        logs.queue = [];
        logs.paint();
        return;
      }
      if ((hit = on("[data-bot-toggle]"))) {
        const body = document.getElementById(hit.getAttribute("aria-controls"));
        const expanded = hit.getAttribute("aria-expanded") !== "true";
        hit.setAttribute("aria-expanded", bool(expanded));
        if (body) body.hidden = !expanded;
        hit.closest(".bot")?.toggleAttribute("data-open", expanded);
        return;
      }
      if (on("[data-global-reset]")) {
        const form = $("#global-form");
        if (!form || !formDirty(form, false)) return;
        const ok = await confirmAction({
          title: "撤销未保存的更改？",
          body: "指令前缀、群聊范围和高级设置会恢复到上次保存的内容。",
          confirm: "撤销更改",
          cancel: "继续编辑",
        });
        if (!ok) return;
        form.dataset.refresh = "replace";
        await render({ quiet: true });
        $("#scope-title")?.focus({ preventScroll: true });
        snackbar("已撤销未保存的更改。");
        return;
      }
      if (on("[data-add-bot]")) {
        if (!$('[data-bot=""]')) {
          $("#bot-empty")?.remove();
          $("#bot-list").insertAdjacentHTML("beforeend", part(botForm({ protocol: "satori", url: "", enabled: true }, "")));
        }
        $("#bot-new-url")?.focus();
        return;
      }
      if ((hit = on("[data-drop-bot]"))) {
        // 草稿只活在页面上；它的序号是空串，绝不能当成第 0 条去删配置。
        if (hit.dataset.dropBot === "") {
          hit.closest("form")?.remove();
          $("[data-add-bot]")?.focus();
          return;
        }
        const index = Number(hit.dataset.dropBot);
        const ok = await confirmAction({
          title: `删除连接 ${index + 1}？`,
          body: "删除后这条连接不再使用，重启后生效。",
          confirm: "删除",
          danger: true,
        });
        if (!ok) return;
        try {
          const reply = await api("/settings/bot", {
            method: "POST",
            body: { index, remove: true, enabled: false, protocol: "satori", url: "" },
          });
          hit.closest("form")?.remove();
          for (const form of $$("[data-bot]")) {
            if (form.dataset.bot !== "" && Number(form.dataset.bot) > index) {
              const oldIndex = form.dataset.bot;
              form.dataset.bot = String(Number(oldIndex) - 1);
              for (const node of [form, ...$$("*", form)]) {
                for (const key of ["id", "for", "aria-controls", "aria-labelledby", "aria-describedby"]) {
                  if (node.hasAttribute(key)) node.setAttribute(key, node.getAttribute(key).replaceAll(`bot-${oldIndex}-`, `bot-${form.dataset.bot}-`));
                }
              }
              $("[data-drop-bot]", form).dataset.dropBot = form.dataset.bot;
            }
          }
          snackbar(reply.message);
          await render({ quiet: true });
        } catch (error) {
          report(error);
        }
        return;
      }
      if (on("[data-copy-start]")) {
        try {
          await navigator.clipboard.writeText("acumen start");
          snackbar("已复制：acumen start");
        } catch {
          snackbar("复制失败，命令是 acumen start", "error");
        }
        return;
      }
      if ((hit = on("[data-restart]"))) return restartBot(hit);
      if ((hit = on("[data-update]"))) return updates.apply(hit);
      if ((hit = on("[data-lifecycle]"))) return lifecycle.run(hit.dataset.lifecycle, hit);
      if ((hit = on("[data-run]"))) {
        const input = $("#command-input");
        if (input) input.value = hit.dataset.run;
        return runCommand(hit.dataset.run);
      }
      if (on("[data-logout]")) {
        const ok = await confirmAction({
          title: "在这台设备上退出？",
          body: "会清除这个浏览器保存的访问口令，下次打开需要重新输入。知微本身继续运行。",
          confirm: "退出",
        });
        if (ok) lock("已退出。");
      }
    });

    document.addEventListener("change", (event) => {
      const target = event.target;
      if (target.name === "scope_mode") {
        const form = target.closest("#global-form");
        syncGlobalScope(form);
        syncGlobalDraft(form);
        return;
      }
      // 相册里选完图：立刻上传，再把这些文件名加进列表。选完就清空，同一张图还能再选一次。
      if (target.matches?.(".photo-file")) {
        const files = [...(target.files || [])];
        target.value = "";
        if (files.length) void pickPhotos(target.closest(".photos"), files);
        return;
      }
      if (target.matches?.(".ratio-input")) return void commitRatio(target);
      if (target.matches?.("input[data-path], textarea[data-path], select[data-path]")) return void commitField(target);
      if (target.matches?.(".tpl-image-file")) return void tplImage(target);
      if (target.matches?.(".rule-modes input")) return ruleModePick(target);
      if (target.matches?.("[data-rule-cooldown]")) return void ruleSave(target.closest("[data-rule]"));
      if (target.id === "rules-try-group") return ruleTry();
      if (target.name === "log-level") {
        logs.level = target.value;
        logs.paint();
      }
    });

    // 用户亲手输入才算编辑（见 typed）。
    document.addEventListener("beforeinput", markTyped);
    // 自动填充会写 value 并补发 input / change，也可能在聚焦时才填：这些都不是用户的输入。
    // 文案编辑器记住最后在写哪一种说法：插入芯片往那里插。
    // 点芯片时不让输入框失焦——手机上键盘不必收起又弹出，光标也还在原处。
    document.addEventListener("mousedown", (event) => {
      if (event.target.closest?.("[data-tpl-insert]") && document.activeElement?.matches?.("[data-tpl-input]")) event.preventDefault();
    });
    document.addEventListener("focusin", (event) => {
      if (event.target.matches?.("[data-tpl-input]")) {
        const root = event.target.closest("[data-tpl]");
        root.dataset.tplActive = event.target.closest(".tpl-item").dataset.tplIndex;
      }
    });
    document.addEventListener("focusin", (event) => {
      if (event.target.name === "access_token") scrubAutofill(event.target.form);
    });

    document.addEventListener("input", (event) => {
      const target = event.target;
      markTyped(event);
      const global = target.closest?.("#global-form");
      if (global) syncGlobalDraft(global);
      if (target.name === "access_token") scrubAutofill(target.form);
      if (target.matches?.("input[data-kind=\"number\"], [data-rule-cooldown]")) {
        const unit = $(`[data-unit-for="${CSS.escape(target.id)}"]`) || $(`[data-seconds-for="${CSS.escape(target.id)}"]`);
        if (unit) unit.textContent = unit.dataset.unit === "ms" ? millisText(target.value) : secondsText(target.value);
      }
      // 概率滑块：拖动时只改填充与右边的数，松手才落盘（change）。
      if (target.matches?.("input[data-kind=\"ratio\"]")) syncRatio(target);
      // 反过来：直接在框里敲数字，滑块与填充跟着走（离开输入框才落盘）。
      if (target.matches?.(".ratio-input") && target.value.trim() !== "") {
        const slider = target.closest(".ratio")?.querySelector(".slider");
        const pct = Math.round(Number(target.value));
        if (slider && Number.isFinite(pct) && pct >= 0 && pct <= 100) paintRatio(slider, pct);
      }
      if (target.id === "rules-try-input") return ruleTry();
      if (target.id === "rules-search") return rulesFilter(target);
      if (target.matches?.(".picker-add input")) {
        const root = target.closest(".picker");
        // 刚加完一个又敲了空格：框里只剩空白就清掉，别留个看不见的前导空格。
        if (target.value && !target.value.trim()) target.value = "";
        syncAddButton(root);
        clearFieldError(target);
        // 打下分隔符就等于「这一个写完了」；输入法拼写中、或夹着认不出的字时不抢着加。
        if (!event.isComposing && /[\s,，、;；]/u.test(target.value) && target.value.trim()) {
          const { ids, rejected } = parseIds(target.value);
          if (ids.length && !rejected.length) pickerSubmitText(root);
        }
      } else if (target.matches?.("[data-tpl-input]")) {
        tplRepaint(target);
        tplGrow(target);
      } else if (target.matches?.(".tags-add input")) {
        syncTagsCommit(target.closest(".tags"));
        clearFieldError(target);
      } else if (target.dataset?.filter !== undefined && target.matches("input[data-filter]")) {
        filterConfig(target);
      } else if (target.id === "plugin-search") {
        plugins.query = target.value;
        plugins.repaint();
      } else if (target.id === "log-search") {
        logs.query = target.value;
        logs.paint();
      } else if (target.matches?.("textarea[data-source]")) onSourceInput(target);
      else if (target.getAttribute?.("aria-invalid") === "true") clearFieldError(target);
    });

    // 整段粘贴（一串群号、带引号的 JSON、从别处复制来的名单）直接拆开加进去，不用先落进输入框再整理。
    // 输入框里已经有字，说明人在手打：那就按普通粘贴走，别把半个群号当成整个。
    document.addEventListener("paste", (event) => {
      const target = event.target;
      // 列表：粘进来的一段有好几行（或一串数字），直接拆成多项加进去。
      if (target.matches?.(".tags-add input") && !target.value) {
        const root = target.closest(".tags");
        const text = event.clipboardData?.getData("text") ?? "";
        const many = root.dataset.type === "number" ? /[\s,，、;；]/u.test(text.trim()) : /\n/.test(text.trim());
        if (!many || root.dataset.editing !== undefined) return;
        event.preventDefault();
        tagsSubmit(root, text);
        return;
      }
      if (!target.matches?.(".picker-add input") || target.value) return;
      const text = event.clipboardData?.getData("text") ?? "";
      if (!parseIds(text).ids.length) return;
      event.preventDefault();
      target.value = text;
      pickerSubmitText(target.closest(".picker"), { explicit: true });
    });

    // 离开输入框即保存，与插件配置里其他格子一致。
    document.addEventListener("focusout", (event) => {
      const target = event.target;
      // 文案编辑器：焦点还在这个编辑器里（换到另一种说法、点了插入芯片）就先不存，整个离开才存。
      const tpl = target.closest?.("[data-tpl]");
      if (tpl) {
        if (!tpl.contains(event.relatedTarget) && tpl.isConnected) void tplSave(tpl);
        return;
      }
      if (target.matches?.(".tags-add input") && target.isConnected) {
        const root = target.closest(".tags");
        // 焦点只是挪到同一个编辑器里的按钮（保存、取消、某一枚芯片）上：不抢着提交。
        if (root.contains(event.relatedTarget)) return;
        if (root.dataset.editing !== undefined) {
          if (target.value.trim() && target.value.trim() !== String(readList(root)[Number(root.dataset.editing)])) tagsSubmit(root);
          else stopEditing(root);
        } else if (target.value.trim()) tagsSubmit(root);
        return;
      }
      if (!target.matches?.(".picker-add input") || !target.isConnected || !target.value.trim()) return;
      pickerSubmitText(target.closest(".picker"));
    });

    document.addEventListener("keydown", (event) => {
      const target = event.target;
      if (event.isComposing) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        const form = target.closest?.("#global-form, [data-bot]") || $("#global-form");
        if (form) {
          event.preventDefault();
          if (formDirty(form, false) && form.getAttribute("aria-busy") !== "true") form.requestSubmit();
          else snackbar(form.getAttribute("aria-busy") === "true" ? "正在保存，请稍等。" : "没有需要保存的更改。");
          return;
        }
      }
      // 插件页的键盘路径：/ 搜索、上下键在结果里走、Esc 退出搜索。
      // 只在插件页生效，也不抢输入框里的按键。
      if ($("#plugin-search") && !document.querySelector("dialog[open]")) {
        const typing = target.matches?.("input, textarea, select, [contenteditable]");
        // 筛选矩阵：方向键只在格子里移动焦点（空格 / 回车才筛），Home / End 接到两头。
        const rail = target.closest?.("#plugin-filters");
        if (rail && !typing && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey) {
          const cards = $$("[data-filter]", rail);
          const index = cards.indexOf(target.closest("[data-filter]"));
          // 用第一行实际摆了几格算列数：2 / 3 / 4 列随容器宽度变，读计算样式不可靠。
          const columns = Math.max(1, cards.filter((card) => card.offsetTop === cards[0]?.offsetTop).length);
          let next = -1;
          if (event.key === "ArrowRight") next = (index + 1) % cards.length;
          else if (event.key === "ArrowLeft") next = (index - 1 + cards.length) % cards.length;
          else if (event.key === "ArrowDown") next = Math.min(index + columns, cards.length - 1);
          else if (event.key === "ArrowUp") next = Math.max(index - columns, 0);
          else if (event.key === "Home") next = 0;
          else if (event.key === "End") next = cards.length - 1;
          if (index >= 0 && next >= 0 && next !== index) {
            event.preventDefault();
            cards[next].focus();
            return;
          }
        }
        if (!typing && !event.ctrlKey && !event.metaKey && !event.altKey && event.key === "/") {
          event.preventDefault();
          $("#plugin-search").focus({ preventScroll: true });
          return;
        }
        if (event.key === "Escape" && target.id === "plugin-search") {
          event.preventDefault();
          if (plugins.query) {
            plugins.query = "";
            target.value = "";
            plugins.repaint();
          } else {
            target.blur();
          }
          return;
        }
        if (!typing && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
          const links = $$("#plugin-list .item-link");
          if (!links.length) return;
          const index = links.indexOf(document.activeElement);
          const next =
            event.key === "ArrowDown"
              ? index < 0
                ? 0
                : Math.min(index + 1, links.length - 1)
              : index <= 0
                ? links.length - 1
                : index - 1;
          event.preventDefault();
          links[next].focus({ preventScroll: true });
          links[next].scrollIntoView({ block: "nearest" });
          return;
        }
      }
      if (target.matches?.(".tags-add input")) {
        const root = target.closest(".tags");
        if (event.key === "Enter") {
          event.preventDefault();
          tagsSubmit(root);
        } else if (event.key === "Escape" && root.dataset.editing !== undefined) {
          event.preventDefault();
          stopEditing(root, { refocus: true });
        } else if (event.key === "Backspace" && !target.value && root.dataset.editing === undefined) {
          // 空框里退格：先跳到最后一枚芯片上（再按一次 Delete / 退格才删），不一下子删掉。
          const chips = $$("[data-tags-edit]", root);
          if (chips.length) {
            event.preventDefault();
            chips[chips.length - 1].focus();
          }
        }
        return;
      }
      if (target.matches?.("[data-tags-edit]")) {
        const root = target.closest(".tags");
        const index = Number(target.dataset.tagsEdit);
        const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[event.key];
        if (step && event.altKey) {
          event.preventDefault();
          tagsMove(root, index, index + step);
        } else if (event.key === "Delete" || event.key === "Backspace") {
          event.preventDefault();
          tagsRemove($(`[data-tags-remove="${index}"]`, root));
        } else if (step && !event.altKey && !event.shiftKey) {
          // 方向键在芯片之间走，与系统的芯片组一致。
          const chips = $$("[data-tags-edit]", root);
          const next = chips[index + step];
          if (next) {
            event.preventDefault();
            next.focus();
          }
        }
        return;
      }
      if (event.key === "Enter" && target.matches?.("input[data-filter], #rules-try-input, #rules-search")) {
        event.preventDefault();
        return;
      }
      if (event.key === "Enter" && target.matches?.("[data-rule-cooldown]")) {
        event.preventDefault();
        void ruleSave(target.closest("[data-rule]"));
        return;
      }
      if (event.key === "Enter" && target.matches?.(".picker-add input")) {
        // 表单里回车默认会提交整张表单；在这里它的意思只是「把这个群号加进去」。
        event.preventDefault();
        pickerSubmitText(target.closest(".picker"), { explicit: true });
        return;
      }
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && target.matches?.("[data-tpl-input]")) {
        event.preventDefault();
        void tplSave(target.closest("[data-tpl]"));
        return;
      }
      if (event.key === "Enter" && target.matches?.(".ratio-input")) {
        // 离开输入框就是「敲完了」：change 会把它收进 0—100 并保存。
        event.preventDefault();
        target.blur();
        return;
      }
      if (event.key === "Enter" && target.matches?.("input[data-path]")) {
        event.preventDefault();
        commitField(target);
        return;
      }
      if (target.getAttribute?.("role") === "tab") {
        const tabs = $$('[role="tab"]');
        const index = tabs.indexOf(target);
        const next = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1 }[event.key];
        if (next === undefined) return;
        event.preventDefault();
        selectTab(tabs[(next + tabs.length) % tabs.length], true);
      }
    });

    document.addEventListener("submit", async (event) => {
      const form = event.target;
      event.preventDefault();
      if (form.id === "lock-form") {
        const value = $("#lock-input").value.trim();
        if (value) unlock(value);
        return;
      }
      if (form.id === "command-form") return runCommand($("#command-input").value);
      if (form.id === "global-form") {
        let refocus = null;
        await submitWith(form, async () => {
          let body;
          try {
            body = globalPayload(form);
          } catch (error) {
            if (error.field) fieldError(error.field, error.message);
            refocus = error.refocus ?? null;
            return;
          }
          // 空白名单等于「什么都不处理」：它藏在一个看起来很安全的分段按钮后面，
          // 所以至少在写下去之前用一句话把后果说清楚。
          if (body.global_filter.enable_whitelist && !body.global_filter.whitelist.length) {
            const ok = await confirmAction({
              title: "白名单还是空的",
              body: "保存后，知微将不处理任何群的消息，直到你添加群或切回其他范围。",
              confirm: "仍然保存",
              cancel: "回去添加",
              danger: true,
            });
            if (!ok) {
              refocus = pickerAdd($('.picker-value[name="whitelist"]', form).closest(".picker"));
              return;
            }
          }
          try {
            const reply = await api("/settings/global", { method: "POST", body });
            for (const input of $$("input", form)) {
              input.defaultValue = input.value;
              clearFieldError(input);
            }
            for (const button of $$("[data-form-switch]", form)) button.dataset.initial = button.getAttribute("aria-checked");
            snackbar(reply.message);
          } catch (error) {
            report(error);
          }
        });
        syncGlobalDraft(form);
        if (refocus) refocus.focus();
        else if (!document.activeElement || document.activeElement === document.body) $("#global-save-state")?.focus({ preventScroll: true });
        return;
      }
      if (form.matches("[data-bot]")) {
        // 跟随模块的连接可以不写地址；其余的 Satori 连接必须写，先在本地挡住，省一个来回。
        const url = form.elements.url;
        if (!form.hasAttribute("data-follow") && form.elements.protocol?.value === "satori" && !url.value.trim()) {
          fieldError(url, "Satori 连接需要一个实现端地址。");
          url.focus();
          return;
        }
        await submitWith(form, async () => {
          try {
            const reply = await api("/settings/bot", { method: "POST", body: botPayload(form) });
            const index = form.dataset.bot;
            form.dataset.refresh = "replace";
            snackbar(reply.message);
            await render({ quiet: true });
            // 表单收起之后，焦点回到这条连接上：键盘用户不必从头再找一遍。
            const back = index === "" ? $$("[data-bot] [data-bot-toggle]").at(-1) : $(`[data-bot="${CSS.escape(index)}"] [data-bot-toggle]`);
            (back ?? $("[data-add-bot]"))?.focus({ preventScroll: true });
          } catch (error) {
            report(error);
          }
        });
      }
    });

    // 规则面板里的列表（触发词、群名单）改了：随整条规则一起存。
    document.addEventListener("listchange", (event) => {
      const entry = event.target.closest?.("[data-rule]");
      if (entry) void ruleSave(entry, event.detail || {});
    });

    // 映射表里展开 / 收起的项记下来：加项、删项后重画这一块时照旧。
    document.addEventListener("toggle", (event) => {
      if (event.target.matches?.("details.rule")) {
        const id = Number(event.target.dataset.rule);
        if (event.target.open) rules.open.add(id);
        else rules.open.delete(id);
        return;
      }
      if (event.target.matches?.("details.rule-more")) {
        const id = Number(event.target.closest("[data-rule]")?.dataset.rule);
        if (event.target.open) rules.more.add(id);
        else rules.more.delete(id);
        return;
      }
      const entry = event.target;
      if (!entry.matches?.("details.entry")) return;
      const key = `${entry.closest("[data-config]")?.dataset.config}:${entry.dataset.entry}`;
      if (entry.open) openEntries.add(key);
      else openEntries.delete(key);
    }, true);

    const host = $("#snackbar");
    host.addEventListener("pointerenter", () => holdSnack(true));
    host.addEventListener("pointerleave", () => holdSnack(false));
    host.addEventListener("focusin", () => holdSnack(true));
    host.addEventListener("focusout", () => holdSnack(false));

    // 进后台就断开日志流、停掉总览轮询；回来由快照补齐。后台零解析、零重排。
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) {
        logs.stop();
        overviewFeed.stop();
      } else if (token) resume();
    });
    addEventListener("pagehide", () => {
      logs.stop();
      overviewFeed.stop();
    });
    addEventListener("pageshow", () => {
      scrubAutofill();
      if (!document.hidden && token) resume();
    });
    addEventListener("hashchange", () => {
      if (undoing) settleUrl();
      else render();
    });
    wide.addEventListener("change", () => { if (shown?.page === "plugins") render({ quiet: true }); });
    addEventListener("beforeunload", (event) => {
      if (hasFormDrafts() || SOURCES.some((source) => dirty(source.name))) event.preventDefault();
    });
  }

  function resume() {
    if ($("#log-box")) logs.mount();
    if ($("#overview-log") && !overviewFeed.timer) overviewFeed.start();
    if (lifecycle.offline && !lifecycle.timer) lifecycle.watch();
    updates.check();
  }

  /* ==================== §14 启动 ==================== */

  document.addEventListener("DOMContentLoaded", () => {
    buildShell();
    bindEvents();
    bindNavDrag();
    bindNavMini();
    bindSwipe();
    bindTagDrag();
    token = readToken();
    if (!token) {
      lock();
      return;
    }
    document.body.dataset.state = "ready";
    // 先问一句是不是经管理面板打开的（本机一来一回，很快），停机页要靠它。
    lifecycle.probe().finally(() => {
      render();
      updates.check();
      appearance.pull();
    });
  });
})();
