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
   §7 总览 · §8 插件 · §9 搭话 · §10 日志 · §11 设置 · §12 解锁 · §13 事件 · §14 启动
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

  /* ==================== §2 图标 ==================== */
  /* 24 网格、圆头圆角描边，笔画粗细取 --zw-icon-stroke。装饰性，对辅助技术隐藏。 */

  const PATHS = {
    overview: '<rect x="3.5" y="3.5" width="7" height="9" rx="2"/><rect x="13.5" y="3.5" width="7" height="5" rx="2"/><rect x="13.5" y="11.5" width="7" height="9" rx="2"/><rect x="3.5" y="15.5" width="7" height="5" rx="2"/>',
    plugins: '<rect x="3.5" y="3.5" width="7" height="7" rx="2"/><rect x="3.5" y="13.5" width="7" height="7" rx="2"/><rect x="13.5" y="13.5" width="7" height="7" rx="2"/><path d="M17 2.9 20.1 6 17 9.1 13.9 6z"/>',
    ambient: '<path d="M20 11.5a7.5 7.5 0 0 1-10.9 6.7L4 19.5l1.3-4.4A7.5 7.5 0 1 1 20 11.5z"/><path d="M8.8 11.5h.01M12.3 11.5h.01M15.8 11.5h.01"/>',
    logs: '<path d="M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-10z"/><path d="M14 3.5v5h5M8.5 13h7M8.5 16.5h5"/>',
    settings: '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
    refresh: '<path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3"/><path d="M19.5 4.5v4h-4"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4 4"/>',
    chevron: '<path d="m9.5 6 6 6-6 6"/>',
    back: '<path d="m14.5 6-6 6 6 6"/>',
    down: '<path d="m6 9.5 6 6 6-6"/>',
    check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
    close: '<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>',
    copy: '<rect x="8.5" y="8.5" width="11" height="11" rx="2.5"/><path d="M15.5 5.5A2 2 0 0 0 13.5 4H6a2 2 0 0 0-2 2v7.5a2 2 0 0 0 1.5 1.94"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    trash: '<path d="M4.5 7h15M9.5 7V5a1.5 1.5 0 0 1 1.5-1.5h2A1.5 1.5 0 0 1 14.5 5v2M6.5 7l.8 11.6a2 2 0 0 0 2 1.9h5.4a2 2 0 0 0 2-1.9L17.5 7"/>',
    download: '<path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14"/>',
    latest: '<path d="M12 5v14M6.5 13.5 12 19l5.5-5.5"/>',
    play: '<path d="M8 5.5v13l10-6.5z"/>',
    alert: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v5.5M12 16.5h.01"/>',
    logout: '<path d="M14 4.5h3.5a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H14M10 16.5 5.5 12 10 7.5M5.5 12h9"/>',
    inbox: '<path d="M4 13.5 6.5 5.5h11l2.5 8v5a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 18.5z"/><path d="M4 13.5h4.5l1.5 2.5h4l1.5-2.5H20"/>',
  };

  const icon = (name, extra = "") =>
    raw(
      `<svg class="i${extra ? ` ${extra}` : ""}" viewBox="0 0 24 24" fill="none" stroke="currentColor" ` +
        `stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${PATHS[name]}</svg>`
    );

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

  const clock = () => new Date().toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
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
      throw failure(payload.error || `服务返回了 ${response.status}`, response.status);
    }
    return payload;
  }

  /** 接口失败的统一出口。401 已经换成解锁页，不再弹提示。 */
  function report(error) {
    if (error?.status === 401) return;
    snackbar(error?.message || String(error), "error");
  }

  /* ==================== §5 反馈 ==================== */

  const snack = { timer: 0, hold: false, left: 0 };

  /** 提示条。成功与进行中是 status（礼貌播报），失败是 alert（立即播报）。
   *  鼠标悬停或键盘聚焦在提示条上时暂停计时（WCAG 2.2.1）。 */
  function snackbar(message, kind = "info") {
    clearTimeout(snack.timer);
    const host = $("#snackbar");
    put(
      host,
      h`<div class="snackbar${kind === "error" ? " snackbar-error" : ""}">
        <span class="snackbar-text" role="${kind === "error" ? "alert" : "status"}">${message}</span>
        <button class="icon-btn state" type="button" data-snack-close aria-label="关闭提示">${icon("close")}</button>
      </div>`
    );
    snack.left = kind === "error" ? 10000 : 4000;
    armSnack();
  }

  function armSnack() {
    clearTimeout(snack.timer);
    if (snack.hold || !$("#snackbar .snackbar")) return;
    const started = Date.now();
    snack.timer = setTimeout(() => put($("#snackbar"), ""), snack.left);
    snack.started = started;
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
  function confirmAction({ title, body, confirm, danger = false }) {
    const dialog = $("#dialog");
    const opener = document.activeElement;
    put(
      dialog,
      h`<h2 class="dialog-title" id="dialog-title">${title}</h2>
        <p class="dialog-body" id="dialog-body">${body}</p>
        <div class="actions actions-end">
          <button class="btn btn-text" type="button" value="cancel" autofocus>取消</button>
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

  const emptyState = (message, name = "inbox") =>
    h`<div class="empty"><span class="empty-mark">${icon(name)}</span><p>${message}</p></div>`;

  const skeleton = () =>
    h`<div class="page" aria-hidden="true"><div class="skeleton">
        <div class="skeleton-block"></div><div class="skeleton-block skeleton-tall"></div>
        <div class="skeleton-block"></div><div class="skeleton-block"></div>
      </div></div>`;

  function pageHead(title, sub = "", { back = null, actions = "" } = {}) {
    return h`<header class="page-head">
      <div class="page-head-text">
        ${back ? h`<a class="btn btn-text back" href="${back.href}">${icon("back")}${back.label}</a>` : ""}
        <h1 class="page-title" tabindex="-1">${title}</h1>
        ${sub ? h`<p class="page-sub">${sub}</p>` : ""}
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

  const PAGES = [
    { id: "overview", label: "总览" },
    { id: "plugins", label: "插件" },
    { id: "ambient", label: "搭话" },
    { id: "logs", label: "日志" },
    { id: "settings", label: "设置" },
  ];

  const wide = matchMedia("(min-width: 840px)");

  function buildShell() {
    put(
      $("#nav"),
      PAGES.map(
        (page) => h`<a class="nav-item" href="#/${page.id}" data-nav="${page.id}">
          <span class="nav-icon state">${icon(page.id)}</span>
          <span class="nav-label">${page.label}</span>
        </a>`
      )
    );
    put(
      $("#topbar-actions"),
      h`<button class="icon-btn state" id="refresh" type="button" aria-label="刷新当前页面" title="刷新当前页面">${icon("refresh")}</button>`
    );
  }

  function markNav(active) {
    for (const item of $$("#nav [data-nav]")) {
      if (item.dataset.nav === active) item.setAttribute("aria-current", "page");
      else item.removeAttribute("aria-current");
    }
  }

  function parseRoute() {
    const [head = "", ...rest] = location.hash.replace(/^#\/?/, "").split("/");
    const page = PAGES.some((p) => p.id === head) ? head : "overview";
    let arg = null;
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
    ambient: () => ambientPage(),
    logs: () => logsPage(),
    settings: () => settingsPage(),
  };

  const MOUNT = {
    overview: () => overviewFeed.start(),
    logs: () => logs.mount(),
  };

  let renderSeq = 0;
  let shown = null;
  const scrolls = new Map();

  /** 画当前路由。`quiet` 用于刷新：不出骨架、不挪焦点、保留滚动位置。 */
  async function render({ quiet = false } = {}) {
    if (!token) {
      lock();
      return;
    }
    const route = parseRoute();
    const previous = shown;
    const changed = !previous || previous.key !== route.key;
    markNav(route.page);
    overviewFeed.stop();
    if (route.page !== "logs") logs.stop();

    // 宽屏插件页：列表已经在屏上，只换右侧详情，搜索词、焦点与滚动都不动。
    if (!quiet && previous?.page === "plugins" && route.page === "plugins" && wide.matches && $("#plugin-detail")) {
      shown = route;
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
    } catch (error) {
      if (seq !== renderSeq || error?.status === 401) return;
      busy(false);
      shown = route;
      put(
        $("#view"),
        h`<div class="page">${pageHead("没能打开这一页")}
          <div class="card"><div class="empty" role="alert">
            <span class="empty-mark">${icon("alert")}</span><p>${error?.message || "原因不明"}</p>
            <button class="btn btn-tonal" type="button" data-retry>${icon("refresh")}重试</button>
          </div></div></div>`
      );
      watchTitle();
      return;
    }
    if (seq !== renderSeq) return;

    const keep = quiet ? scrollY : changed ? scrolls.get(route.key) || 0 : scrollY;
    shown = route;
    busy(false);
    put($("#view"), fragment);
    const page = $("#view > .page");
    if (changed && page) page.toggleAttribute("data-enter", true);
    const heading = $(".page-title");
    if (heading) setTitle(page?.dataset.title || heading.textContent.trim());
    scrollTo({ top: keep, behavior: "instant" });
    watchTitle();
    MOUNT[route.page]?.();
    // 换页后把焦点交给新页的标题：读屏从这里开始读，键盘从这里继续走。
    if (changed && previous && !quiet) heading?.focus({ preventScroll: true });
  }

  /* ==================== §7 总览 ==================== */

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
        const data = await api("/logs?limit=5");
        const html = part((data.lines || []).slice(-5).map(logRow)) || part(h`<p class="log-empty">还没有日志</p>`);
        if (box.dataset.digest !== html) {
          box.dataset.digest = html;
          box.innerHTML = html;
        }
      } catch {
        if (!box.children.length) put(box, h`<p class="log-empty">暂时读不到日志</p>`);
      }
    },
  };

  async function overviewPage() {
    const data = await api("/overview");
    const stat = (label, value, unit) =>
      h`<div class="stat"><dt class="stat-label">${label}</dt>
        <dd class="stat-value"><span class="num">${value}</span>${unit ? h`<span class="stat-unit">${unit}</span>` : ""}</dd></div>`;
    const bots = data.bots.length
      ? h`<ul class="group">${data.bots.map(
          (bot) => h`<li class="item">
            <span class="item-leading" aria-hidden="true">${(bot.name || bot.nick || "?").slice(0, 1)}</span>
            <div class="item-content">
              <span class="item-title">${bot.name || bot.nick || "未识别的账号"}<span class="tag">${bot.adapter}/${bot.platform}</span></span>
              <span class="item-sub">${bot.id ? `账号 ${bot.id}` : "已连接实现端，账号尚未识别"}</span>
            </div>
            <div class="item-trailing"><span class="status status-success">已连接</span></div>
          </li>`
        )}</ul>`
      : h`<div class="card">${emptyState("还没有连接到任何实现端。在「设置」里添加一条连接，重启后生效。")}</div>`;
    const pending = data.plugins.pending ? `，${data.plugins.pending} 个待重启` : "";

    return h`<div class="page">
      ${pageHead("总览", "这台设备上知微的运行情况。")}
      <div class="overview">
        <section class="hero" aria-labelledby="hero-label">
          <span class="hero-art" aria-hidden="true"></span>
          <div class="hero-top"><span>${NAME} v${data.app.version}</span><span class="status status-success">运行中</span></div>
          <h2 class="hero-label" id="hero-label">已连续运行</h2>
          <p class="hero-value" data-uptime="${data.app.uptime}">${duration(data.app.uptime)}</p>
          <p class="hero-note">启动于 <time>${data.app.started}</time></p>
          <div class="actions">
            <a class="btn btn-filled" href="#/logs">${icon("logs")}查看日志</a>
            <a class="btn btn-outlined" href="#/plugins">${icon("plugins")}管理插件</a>
          </div>
        </section>

        <section class="section" aria-labelledby="bots-title">
          <div class="section-head">
            <h2 class="section-title" id="bots-title">连接</h2>
            <a class="btn btn-text" href="#/settings">管理连接</a>
          </div>
          ${bots}
        </section>

        <section class="section section-wide" aria-labelledby="stats-title">
          <div class="section-head">
            <h2 class="section-title" id="stats-title">消息与插件</h2>
            <span class="section-meta">更新于 <time>${clock()}</time></span>
          </div>
          <dl class="stats">
            ${stat("今日消息", num(data.messages.today), "条")}
            ${stat("今日发言", num(data.messages.people), "人")}
            ${stat("近 7 天消息", num(data.messages.week), "条")}
            ${stat("已启用插件", `${data.plugins.on} / ${data.plugins.total}`, pending.slice(1))}
          </dl>
        </section>

        <section class="section section-wide" aria-labelledby="recent-title">
          <div class="section-head">
            <h2 class="section-title" id="recent-title">最近日志</h2>
            <a class="btn btn-text" href="#/logs">全部日志${icon("chevron", "i-end")}</a>
          </div>
          <div class="log log-short" id="overview-log"></div>
        </section>
      </div>
    </div>`;
  }

  /* ==================== §8 插件 ==================== */

  const plugins = {
    data: null,
    query: "",
    section: "",
    selected: null,
    detailSeq: 0,

    visible() {
      const text = this.query.trim().toLowerCase();
      return this.data.plugins.filter(
        (plugin) =>
          (!this.section || plugin.section === this.section) &&
          (!text ||
            plugin.name.toLowerCase().includes(text) ||
            plugin.display.toLowerCase().includes(text) ||
            plugin.summary.toLowerCase().includes(text))
      );
    },

    rows() {
      const list = this.visible();
      if (!list.length) return h`<li>${emptyState("没有符合条件的插件。换个关键词或筛选试试。", "search")}</li>`;
      const dual = wide.matches;
      return list.map(
        (plugin) => h`<li class="item" data-plugin="${plugin.name}">
          <div class="item-content">
            <a class="item-link item-title" href="#/plugins/${encodeURIComponent(plugin.name)}"${attr(plugin.name === this.selected, 'aria-current="page"')}>
              ${plugin.display}<span class="tag">${plugin.name}</span>${plugin.pending ? h`<span class="status status-warning">待重启</span>` : ""}
            </a>
            <span class="item-sub">${plugin.summary}</span>
          </div>
          <div class="item-trailing">
            ${switchButton(plugin, { label: `启用 ${plugin.display}` })}
            ${dual ? "" : icon("chevron", "chevron")}
          </div>
        </li>`
      );
    },

    chips() {
      const counts = new Map();
      for (const plugin of this.data.plugins) counts.set(plugin.section, (counts.get(plugin.section) || 0) + 1);
      const chip = (code, label, total) =>
        h`<button class="chip state" type="button" data-section="${code}" aria-pressed="${this.section === code}">
          ${icon("check")}${label}<span class="chip-count">${total}</span></button>`;
      return [
        chip("", "全部", this.data.plugins.length),
        ...this.data.sections.filter((s) => counts.get(s.code)).map((s) => chip(s.code, s.name, counts.get(s.code))),
      ];
    },

    summary() {
      const on = this.data.plugins.filter((p) => p.on).length;
      const shownCount = this.visible().length;
      return shownCount === this.data.plugins.length
        ? `共 ${this.data.plugins.length} 个，已启用 ${on} 个`
        : `显示 ${shownCount} / ${this.data.plugins.length} 个`;
    },

    /** 只重画列表与计数：打一个字就整页重来会把输入法与光标一起冲掉。 */
    repaint() {
      put($("#plugin-list"), this.rows());
      put($("#plugin-chips"), this.chips());
      const status = $("#plugin-count");
      if (status) status.textContent = this.summary();
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
        put(pane, h`<div class="card">${emptyState(error.message, "alert")}</div>`);
      } finally {
        if (seq === this.detailSeq) pane.removeAttribute("aria-busy");
      }
    },
  };

  /** 插件开关。名字固定为「启用 某某」，状态交给 aria-checked：开关的名字不随状态变。 */
  const switchButton = (plugin, { label = "", labelledby = "", describedby = "" }) =>
    h`<button class="switch" type="button" role="switch" data-toggle="${plugin.name}" aria-checked="${plugin.on}"
        ${label ? h`aria-label="${label}"` : h`aria-labelledby="${labelledby}" aria-describedby="${describedby}"`}
        ${attr(plugin.name === "ctl", 'disabled title="控制插件不能停用"')}></button>`;

  const detailPlaceholder = () =>
    h`<div class="card">${emptyState("在左侧选择一个插件，这里会显示它的开关、配置与指令。", "plugins")}</div>`;

  async function pluginsPage(route) {
    const [list, detail] = await Promise.all([
      api("/plugins"),
      route.arg ? api(`/plugins/${encodeURIComponent(route.arg)}`).catch((error) => error) : null,
    ]);
    plugins.data = list;
    plugins.selected = route.arg;
    const dual = wide.matches;
    if (detail instanceof Error && detail.status === 401) throw detail;

    // 窄屏：详情单独成页，左上角是返回（HIG 的层级导航）。
    if (route.arg && !dual) {
      if (detail instanceof Error) throw detail;
      return h`<div class="page">
        ${pageHead(detail.display, detail.summary, { back: { href: "#/plugins", label: "插件" } })}
        ${detailBody(detail, 1)}
      </div>`;
    }

    const pane = !dual
      ? ""
      : h`<section class="split-detail" id="plugin-detail" aria-label="插件详情">${
          !detail
            ? detailPlaceholder()
            : detail instanceof Error
              ? h`<div class="card">${emptyState(detail.message, "alert")}</div>`
              : detailBody(detail, 2)
        }</section>`;

    const title = detail && !(detail instanceof Error) ? detail.display : "";
    return h`<div class="page"${title ? h` data-title="${title}"` : ""}>
      ${pageHead("插件", "开关即时生效；配置修改后立即保存。")}
      <div class="split"${attr(dual, "data-dual")}>
        <div class="split-list">
          <div class="toolbar">
            <div class="search">${icon("search")}
              <input class="input" id="plugin-search" type="search" placeholder="搜索名称或说明"
                aria-label="搜索插件" autocomplete="off" value="${plugins.query}">
            </div>
          </div>
          <div class="chips" id="plugin-chips" role="group" aria-label="按分类筛选">${plugins.chips()}</div>
          <p class="section-meta" id="plugin-count" role="status">${plugins.summary()}</p>
          <ul class="group" id="plugin-list">${plugins.rows()}</ul>
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
    const fields = Object.entries(plugin.config)
      .filter(([key]) => key !== "enabled")
      .map(([key, value]) => configField(key, key, value));
    const commands = plugin.commands.length
      ? h`<ul class="group">${plugin.commands.map(
          (command) => h`<li><button class="item" type="button" data-copy="${command.cmd}" aria-label="复制指令 ${command.cmd}">
            <span class="item-content"><span class="item-title"><code class="tag">${command.cmd}</code></span>
            <span class="item-sub">${command.note}</span></span>
            <span class="item-trailing">${icon("copy")}</span></button></li>`
        )}</ul>`
      : h`<p class="note">这个插件没有指令，它在后台按计划工作。</p>`;

    return h`
      ${level === 2
        ? h`<div class="detail-head">
            ${raw(`<${top} class="detail-title" id="detail-title">`)}${plugin.display}<span class="tag">${plugin.name}</span>${statusTag(plugin)}${raw(`</${top}>`)}
            <p class="note">${plugin.summary}</p>
          </div>`
        : h`<div>${statusTag(plugin)}</div>`}
      <div class="card">
        <div class="toggle-row">
          <div class="field">
            <span class="field-label" id="enable-label">启用</span>
            <span class="field-hint" id="enable-hint">${plugin.effect}</span>
          </div>
          ${switchButton(plugin, { labelledby: "enable-label", describedby: "enable-hint" })}
        </div>
      </div>

      <section class="section" aria-labelledby="config-title">
        <div class="section-head">
          ${heading(sub, "config-title", "配置")}
          <span class="section-meta">修改后离开输入框或按回车即保存</span>
        </div>
        ${fields.length
          ? h`<form class="card config" data-config="${plugin.name}" novalidate>${fields}</form>`
          : h`<p class="note">没有可以调整的配置项。</p>`}
        ${fields.length
          ? h`<div class="actions"><button class="btn btn-outlined btn-danger" type="button" data-reset="${plugin.name}" data-display="${plugin.display}">恢复默认参数</button></div>`
          : ""}
      </section>

      <section class="section" aria-labelledby="diff-title">
        <div class="section-head">${heading(sub, "diff-title", "与默认值的差异")}</div>
        <div id="plugin-diff">${diffBlock(plugin.diff)}</div>
      </section>

      <section class="section" aria-labelledby="commands-title">
        <div class="section-head">
          ${heading(sub, "commands-title", "指令")}
          <span class="section-meta">${plugin.commands.length ? `${plugin.commands.length} 条，点按复制` : ""}</span>
        </div>
        ${commands}
      </section>`;
  }

  const diffBlock = (diff) =>
    diff.length
      ? h`<pre class="code" tabindex="0" aria-label="与默认值的差异">${diff.join("\n")}</pre>`
      : h`<p class="note">全部与默认值一致。</p>`;

  let fieldSeq = 0;

  /** 一个配置项。表与对象数组展开成 fieldset，叶子就地成为能改的控件。 */
  function configField(path, key, value) {
    const id = `f${++fieldSeq}`;
    if (Array.isArray(value) && value.every((item) => item === null || typeof item !== "object")) {
      return h`<div class="field">
        <label class="field-label" for="${id}"><code>${key}</code></label>
        <input class="input mono" id="${id}" data-path="${path}" data-kind="list" value="${value.join(", ")}"
          spellcheck="false" autocapitalize="off" aria-describedby="${id}-hint">
        <span class="field-hint" id="${id}-hint">多个值用逗号分隔；留空表示空列表</span>
      </div>`;
    }
    if (value !== null && typeof value === "object") {
      const children = Array.isArray(value)
        ? value.map((item, index) => configField(`${path}.${index}`, `[${index}]`, item))
        : Object.entries(value).map(([child, item]) => configField(`${path}.${child}`, child, item));
      return h`<fieldset class="config-group"><legend class="config-legend">${key}</legend>${children}</fieldset>`;
    }
    if (typeof value === "boolean") {
      return h`<div class="toggle-row">
        <span class="field-label" id="${id}"><code>${key}</code></span>
        <button class="switch" type="button" role="switch" data-path="${path}" data-kind="bool"
          aria-checked="${value}" aria-labelledby="${id}"></button>
      </div>`;
    }
    if (typeof value === "string" && (value.includes("\n") || value.length > 80)) {
      return h`<div class="field">
        <label class="field-label" for="${id}"><code>${key}</code></label>
        <textarea class="textarea" id="${id}" data-path="${path}" data-kind="string" spellcheck="false">${value}</textarea>
      </div>`;
    }
    const numeric = typeof value === "number";
    return h`<div class="field">
      <label class="field-label" for="${id}"><code>${key}</code></label>
      <input class="input${numeric ? " num" : " mono"}" id="${id}" data-path="${path}" data-kind="${numeric ? "number" : "string"}"
        value="${value ?? ""}" spellcheck="false" autocapitalize="off"${attr(numeric, 'inputmode="decimal"')}>
    </div>`;
  }

  function parseValue(kind, text) {
    if (kind === "number") {
      const value = Number(text.trim());
      if (!text.trim() || !Number.isFinite(value)) throw new Error("这一项需要一个数字。");
      return value;
    }
    if (kind !== "list") return text;
    const trimmed = text.trim();
    if (!trimmed) return [];
    return trimmed.split(/[,，]/).map((piece) => {
      const item = piece.trim();
      if (/^-?\d+$/.test(item)) return Number.parseInt(item, 10);
      if (/^-?\d*\.\d+$/.test(item)) return Number.parseFloat(item);
      return item.replace(/^["']|["']$/g, "");
    });
  }

  /** 行内校验（Carbon inline notification）：错在哪一格就在哪一格下面说，并立即播报。 */
  function fieldError(control, message) {
    clearFieldError(control);
    control.setAttribute("aria-invalid", "true");
    const note = document.createElement("p");
    note.className = "field-error";
    note.id = `${control.id}-error`;
    note.setAttribute("role", "alert");
    note.innerHTML = part(h`${icon("alert")}<span>${message}</span>`);
    control.after(note);
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

  async function commitField(control) {
    const form = control.closest("[data-config]");
    if (!form || control.getAttribute("aria-busy") === "true") return;
    const kind = control.dataset.kind;
    let value;
    if (kind === "bool") value = control.getAttribute("aria-checked") !== "true";
    else {
      if (control.value === control.defaultValue) return;
      try {
        value = parseValue(kind, control.value);
      } catch (error) {
        fieldError(control, error.message);
        return;
      }
    }
    control.setAttribute("aria-busy", "true");
    try {
      const reply = await api(`/plugins/${encodeURIComponent(form.dataset.config)}/config`, {
        method: "POST",
        body: { path: control.dataset.path, value },
      });
      if (kind === "bool") control.setAttribute("aria-checked", String(value));
      else control.defaultValue = control.value;
      clearFieldError(control);
      snackbar(reply.message);
      refreshDiff(form.dataset.config);
    } catch (error) {
      if (error.status === 401) return;
      if (kind === "bool") report(error);
      else fieldError(control, error.message);
    } finally {
      control.removeAttribute("aria-busy");
    }
  }

  async function refreshDiff(name) {
    try {
      const plugin = await api(`/plugins/${encodeURIComponent(name)}`);
      if ($(`[data-config="${CSS.escape(name)}"]`)) put($("#plugin-diff"), diffBlock(plugin.diff));
    } catch {
      /* 差异只是参考信息，读不到不打扰 */
    }
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
      const counter = $("#plugin-count");
      if (counter && plugins.data) counter.textContent = plugins.summary();
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
      if (wide.matches && $("#plugin-detail")) await plugins.swapDetail(name);
      else await render({ quiet: true });
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

  /* ==================== §9 搭话 ==================== */

  const SOURCES = [
    { name: "persona", file: "persona.md", title: "人格", note: "它在群里说话的方式与分寸。" },
    { name: "self", file: "self.md", title: "档案", note: "它对自己是谁的认识。" },
  ];
  const STICKER_PAGE = 60;
  const ambient = { data: null, tab: "persona", drafts: {}, stickers: STICKER_PAGE };

  const sourceText = (name) => ambient.drafts[name] ?? ambient.data?.[name] ?? "";
  const dirty = (name) => name in ambient.drafts && ambient.drafts[name] !== (ambient.data?.[name] ?? "");

  function editorState(name) {
    return `${num(count(sourceText(name)))} 字 · ${dirty(name) ? "有未保存的修改" : "已保存"}`;
  }

  async function ambientPage() {
    const data = await api("/ambient");
    ambient.data = data;
    if (!data.ready) {
      return h`<div class="page">${pageHead("搭话")}
        <div class="card">${emptyState("搭话插件的数据目录还没有建立。让它运行一轮之后再来。")}</div></div>`;
    }
    const tabs = [
      { id: "persona", label: "人设" },
      { id: "memory", label: "记忆", count: data.memory.length },
      { id: "stickers", label: "表情包", count: data.stickers.length },
    ];
    return h`<div class="page">
      ${pageHead("搭话", "人设、它记住的人与事，以及收藏的表情包。")}
      <div>
        <div class="tabs" role="tablist" aria-label="搭话内容">
          ${tabs.map(
            (tab) => h`<button class="tab state" type="button" role="tab" id="tab-${tab.id}" data-tab="${tab.id}"
              aria-controls="panel-${tab.id}" aria-selected="${ambient.tab === tab.id}" tabindex="${ambient.tab === tab.id ? 0 : -1}">
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
      </div>
    </div>`;
  }

  function sourceEditor(source) {
    return h`<section class="card editor" aria-labelledby="source-${source.name}-title">
      <div class="section-head">
        <h2 class="section-title" id="source-${source.name}-title">${source.title}</h2>
        <code class="tag">${source.file}</code>
      </div>
      <p class="note" id="source-${source.name}-note">${source.note}保存后下一轮对话生效，旧版本会备份在同一目录。</p>
      <textarea class="textarea textarea-tall" id="source-${source.name}" data-source="${source.name}" spellcheck="false"
        aria-labelledby="source-${source.name}-title" aria-describedby="source-${source.name}-note">${sourceText(source.name)}</textarea>
      <div class="editor-foot">
        <span class="editor-state" id="source-${source.name}-state"${attr(dirty(source.name), "data-dirty")}>${editorState(source.name)}</span>
        <button class="btn btn-filled" type="button" data-save-source="${source.name}"${attr(!dirty(source.name), "disabled")}>保存${source.title}</button>
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
          <span class="item-leading" aria-hidden="true">群</span>
          <span class="item-content">
            <span class="item-title">群 ${group.group}</span>
            <span class="item-sub">${group.people.length} 人 · ${group.notes.length} 条旧事</span>
          </span>
          ${icon("chevron", "chevron")}
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
    $(`[data-save-source="${name}"]`).disabled = !dirty(name);
  }

  async function saveSource(button) {
    const name = button.dataset.saveSource;
    const text = sourceText(name);
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    try {
      const reply = await api("/ambient/source", { method: "POST", body: { name, text } });
      ambient.data[name] = text;
      delete ambient.drafts[name];
      const state = $(`#source-${name}-state`);
      state.textContent = editorState(name);
      state.removeAttribute("data-dirty");
      snackbar(reply.message);
    } catch (error) {
      button.disabled = false;
      report(error);
    } finally {
      button.removeAttribute("aria-busy");
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
      ${pageHead("日志", "实时运行记录。向上滚动会暂停跟随，滚回底部继续。")}
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
            <button class="btn ${logs.follow ? "btn-tonal" : "btn-outlined"}" type="button" id="log-follow" aria-pressed="${logs.follow}">${icon("latest")}跟随最新</button>
            <button class="icon-btn state" type="button" id="log-export" aria-label="导出当前筛选的日志" title="导出当前筛选的日志">${icon("download")}</button>
            <button class="icon-btn state" type="button" id="log-clear" aria-label="清空屏幕上的日志" title="清空屏幕上的日志">${icon("trash")}</button>
          </div>
        </div>
        <div class="log-meta"><span id="log-status" role="status">${logs.status}</span><span id="log-count"></span></div>
        <div class="log-frame">
          <div class="log" id="log-box" tabindex="0" role="region" aria-label="运行日志，最近 ${DOM_LINES} 条"></div>
          <button class="btn btn-filled jump" type="button" id="log-jump"${attr(logs.follow, "hidden")}>${icon("latest")}<span>回到最新</span></button>
        </div>
      </section>
      <p class="note">页面切到后台时暂停接收，回来后自动补齐。完整日志请在本机运行 <code>./bot logs</code>。</p>
    </div>`;
  }

  /* ==================== §11 设置 ==================== */

  const COMMANDS = ["list", "show ambient", "diff oai", "defaults stats"];
  const command = { output: "", history: [], busy: false };

  async function settingsPage() {
    const [data, overview] = await Promise.all([api("/settings"), api("/overview").catch(() => null)]);
    const list = (values) => values.join(", ");
    const filter = data.global_filter;
    return h`<div class="page">
      ${pageHead("设置", "连接、全局行为与维护命令。这些项目不属于任何插件。")}

      <section class="card" aria-labelledby="bots-title">
        <div class="section-head">
          <h2 class="section-title" id="bots-title">连接</h2>
          <span class="section-meta">修改后重启生效</span>
        </div>
        <p class="note">知微通过实现端接入 QQ，例如本机的 <code>http://127.0.0.1:3001</code>。</p>
        <div class="group" id="bot-list">
          ${data.bots.length ? data.bots.map((bot, index) => botForm(bot, String(index))) : h`<p class="note" id="bot-empty">还没有连接。</p>`}
        </div>
        <div class="actions"><button class="btn btn-tonal" type="button" data-add-bot>${icon("plus")}添加连接</button></div>
      </section>

      <section class="card" aria-labelledby="global-title">
        <div class="section-head">
          <h2 class="section-title" id="global-title">全局</h2>
          <span class="section-meta">前缀与名单下一条消息生效</span>
        </div>
        <form id="global-form" class="form-grid" novalidate>
          ${textField("g-prefix", "指令前缀", "command_prefix", list(data.command_prefix), "多个用逗号分隔，例如 /, #")}
          ${textField("g-browser", "浏览器路径", "browser_path", data.browser_path, "留空时自动查找；下次启动生效")}
          <div class="field">
            <div class="toggle-row"><span class="field-label" id="g-black-label">启用群黑名单</span>
              <button class="switch" type="button" role="switch" data-form-switch name="enable_blacklist" aria-checked="${filter.enable_blacklist}" aria-labelledby="g-black-label"></button></div>
            ${textField("g-black", "黑名单群号", "blacklist", list(filter.blacklist), "逗号分隔")}
          </div>
          <div class="field">
            <div class="toggle-row"><span class="field-label" id="g-white-label">启用群白名单</span>
              <button class="switch" type="button" role="switch" data-form-switch name="enable_whitelist" aria-checked="${filter.enable_whitelist}" aria-labelledby="g-white-label"></button></div>
            ${textField("g-white", "白名单群号", "whitelist", list(filter.whitelist), "逗号分隔")}
          </div>
          <p class="note span-all">两个名单都为空时对所有群生效；同时出现在两边的群按禁止处理。</p>
          <div class="actions span-all"><button class="btn btn-filled" type="submit">保存全局设置</button></div>
        </form>
      </section>

      <section class="card" aria-labelledby="command-title">
        <div class="section-head">
          <h2 class="section-title" id="command-title">维护命令</h2>
          <span class="section-meta">与群里的 /ctl 相同，以维护者身份执行</span>
        </div>
        <form class="command-form" id="command-form">
          <label class="visually-hidden" for="command-input">控制命令</label>
          <input class="input mono" id="command-input" autocomplete="off" spellcheck="false" autocapitalize="off"
            enterkeyhint="go" placeholder="list · show ambient · set oai …">
          <button class="btn btn-filled" type="submit">${icon("play")}执行</button>
        </form>
        <div class="chips" role="group" aria-label="常用命令">
          ${[...new Set([...command.history, ...COMMANDS])].slice(0, 6).map(
            (item) => h`<button class="chip state" type="button" data-run="${item}">${item}</button>`
          )}
        </div>
        <pre class="code" id="command-output" role="status" tabindex="0" aria-label="命令回执">${command.output || "执行结果会显示在这里。"}</pre>
      </section>

      <section class="card" aria-labelledby="about-title">
        <div class="section-head"><h2 class="section-title" id="about-title">本机</h2></div>
        <dl class="facts">
          ${overview
            ? h`<div class="fact"><dt>版本</dt><dd>${NAME} v${overview.app.version}</dd></div>
                <div class="fact"><dt>控制台地址</dt><dd><code>${overview.console.address}</code></dd></div>
                <div class="fact"><dt>本次启动</dt><dd><time>${overview.app.started}</time></dd></div>`
            : ""}
          <div class="fact"><dt>安装为应用</dt><dd>${installHint()}</dd></div>
        </dl>
        <div class="actions">
          <button class="btn btn-outlined" type="button" data-logout>${icon("logout")}在这台设备上退出</button>
        </div>
      </section>
    </div>`;
  }

  function installHint() {
    const standalone = matchMedia("(display-mode: standalone)").matches || navigator.standalone === true;
    if (standalone) return "已作为应用打开。";
    if (/iPhone|iPad|iPod/.test(navigator.userAgent)) return "在 Safari 中点「分享」，再选「添加到主屏幕」。";
    if (/Android/.test(navigator.userAgent)) return "在浏览器菜单中选择「安装应用」或「添加到主屏幕」。";
    return "在浏览器地址栏右侧或菜单中选择「安装」。";
  }

  const textField = (id, label, name, value, hint, extra = "") =>
    h`<div class="field">
      <label class="field-label" for="${id}">${label}</label>
      <input class="input mono" id="${id}" name="${name}" value="${value}" spellcheck="false" autocapitalize="off"
        autocomplete="off" aria-describedby="${id}-hint"${raw(extra)}>
      <span class="field-hint" id="${id}-hint">${hint}</span>
    </div>`;

  function botForm(bot, index) {
    const id = `bot-${index === "" ? "new" : index}`;
    const known = ["satori", "console"];
    const protocols = known.includes(bot.protocol) ? known : [...known, bot.protocol];
    const title = index === "" ? "新连接" : `连接 ${Number(index) + 1}`;
    return h`<form class="bot" data-bot="${index}" aria-labelledby="${id}-title" novalidate>
      <div class="bot-head">
        <h3 class="bot-title" id="${id}-title">${title}${bot.has_token ? h`<span class="status">已设令牌</span>` : ""}</h3>
        <div class="toggle-row">
          <span class="field-label" id="${id}-on">启用</span>
          <button class="switch" type="button" role="switch" data-form-switch name="enabled" aria-checked="${bot.enabled}" aria-labelledby="${id}-on"></button>
        </div>
      </div>
      <div class="form-grid">
        <div class="field">
          <label class="field-label" for="${id}-protocol">协议</label>
          <span class="select"><select id="${id}-protocol" name="protocol">
            ${protocols.map((p) => h`<option value="${p}"${attr(p === bot.protocol, "selected")}>${p === "satori" ? "Satori" : p === "console" ? "本地终端（测试用）" : p}</option>`)}
          </select>${icon("down")}</span>
        </div>
        ${textField(`${id}-url`, "实现端地址", "url", bot.url, "Satori 必填，例如 http://127.0.0.1:3001", ' inputmode="url"')}
        <div class="field span-all">
          <label class="field-label" for="${id}-token">访问令牌</label>
          <input class="input mono" id="${id}-token" name="access_token" type="password" autocomplete="off" spellcheck="false"
            placeholder="${bot.has_token ? "已设置；留空保持不变" : "留空表示不鉴权"}">
        </div>
      </div>
      <div class="actions">
        <button class="btn btn-filled" type="submit">保存</button>
        <button class="btn btn-text btn-danger" type="button" data-drop-bot="${index}">${icon("trash")}${index === "" ? "放弃" : "删除"}</button>
      </div>
    </form>`;
  }

  function botPayload(form) {
    const value = (name) => form.elements[name]?.value ?? "";
    const access = value("access_token");
    return {
      index: form.dataset.bot === "" ? undefined : Number(form.dataset.bot),
      enabled: $('[name="enabled"]', form)?.getAttribute("aria-checked") === "true",
      protocol: value("protocol"),
      url: value("url"),
      ...(access.trim() ? { access_token: access } : {}),
    };
  }

  function globalPayload(form) {
    const value = (name) => form.elements[name]?.value ?? "";
    const split = (name) =>
      value(name)
        .split(/[,，]/)
        .map((piece) => piece.trim())
        .filter(Boolean);
    const groups = (name) =>
      split(name).map((piece) => {
        const id = Number(piece);
        if (!Number.isInteger(id)) throw Object.assign(new Error(`「${piece}」不是群号`), { field: form.elements[name] });
        return id;
      });
    const flag = (name) => $(`[name="${name}"]`, form)?.getAttribute("aria-checked") === "true";
    return {
      command_prefix: split("command_prefix"),
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
    try {
      await work();
    } finally {
      button?.removeAttribute("aria-busy");
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
        <img class="lock-mark" src="/icon.svg" alt="" width="64" height="64">
        <h1 class="lock-title page-title" tabindex="-1">解锁知微控制台</h1>
        <p class="note">${reason || "需要访问口令才能查看这台设备的数据。"}
          启动日志里带 <code>?t=</code> 的地址可以直接打开；口令也保存在 <code>data/console/token</code>。</p>
        <input type="text" name="username" autocomplete="username" value="acumen" hidden>
        <div class="field">
          <label class="field-label" for="lock-input">访问口令</label>
          <input class="input mono" id="lock-input" name="password" type="password" autocomplete="current-password"
            required spellcheck="false" autocapitalize="off">
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
    render();
  }

  /* ==================== §13 事件 ==================== */

  function bindEvents() {
    document.addEventListener("click", async (event) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const on = (selector) => target.closest(selector);
      let hit;

      if (on("[data-snack-close]")) {
        clearTimeout(snack.timer);
        put($("#snackbar"), "");
        return;
      }
      if (on("#refresh")) {
        const button = $("#refresh");
        if (button.dataset.busy !== undefined) return;
        button.dataset.busy = "";
        await render({ quiet: true }).finally(() => delete button.dataset.busy);
        return;
      }
      if (on("[data-retry]")) return render();
      if ((hit = on("[data-toggle]"))) return togglePlugin(hit);
      if ((hit = on('button[data-path][data-kind="bool"]'))) return commitField(hit);
      if ((hit = on("[data-form-switch]"))) {
        hit.setAttribute("aria-checked", String(hit.getAttribute("aria-checked") !== "true"));
        return;
      }
      if ((hit = on("[data-section]"))) {
        plugins.section = hit.dataset.section;
        plugins.repaint();
        $(`[data-section="${CSS.escape(plugins.section)}"]`)?.focus();
        return;
      }
      if ((hit = on("[data-reset]"))) return resetPlugin(hit);
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
          snackbar(reply.message);
          await render({ quiet: true });
        } catch (error) {
          report(error);
        }
        return;
      }
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
      if (target.matches?.("input[data-path], textarea[data-path]")) return void commitField(target);
      if (target.name === "log-level") {
        logs.level = target.value;
        logs.paint();
      }
    });

    document.addEventListener("input", (event) => {
      const target = event.target;
      if (target.id === "plugin-search") {
        plugins.query = target.value;
        plugins.repaint();
      } else if (target.id === "log-search") {
        logs.query = target.value;
        logs.paint();
      } else if (target.matches?.("textarea[data-source]")) onSourceInput(target);
      else if (target.getAttribute?.("aria-invalid") === "true") clearFieldError(target);
    });

    document.addEventListener("keydown", (event) => {
      const target = event.target;
      if (event.isComposing) return;
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
        await submitWith(form, async () => {
          let body;
          try {
            body = globalPayload(form);
          } catch (error) {
            if (error.field) fieldError(error.field, error.message);
            return;
          }
          try {
            const reply = await api("/settings/global", { method: "POST", body });
            for (const input of $$("input", form)) {
              input.defaultValue = input.value;
              clearFieldError(input);
            }
            snackbar(reply.message);
          } catch (error) {
            report(error);
          }
        });
        return;
      }
      if (form.matches("[data-bot]")) {
        await submitWith(form, async () => {
          try {
            const reply = await api("/settings/bot", { method: "POST", body: botPayload(form) });
            snackbar(reply.message);
            await render({ quiet: true });
          } catch (error) {
            report(error);
          }
        });
      }
    });

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
      if (!document.hidden && token) resume();
    });
    addEventListener("hashchange", () => render());
    wide.addEventListener("change", () => render({ quiet: true }));
    addEventListener("beforeunload", (event) => {
      if (SOURCES.some((source) => dirty(source.name))) event.preventDefault();
    });
  }

  function resume() {
    if ($("#log-box")) logs.mount();
    if ($("#overview-log") && !overviewFeed.timer) overviewFeed.start();
  }

  /* ==================== §14 启动 ==================== */

  document.addEventListener("DOMContentLoaded", () => {
    buildShell();
    bindEvents();
    token = readToken();
    if (!token) {
      lock();
      return;
    }
    document.body.dataset.state = "ready";
    render();
  });
})();
