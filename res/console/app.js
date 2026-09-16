/* ============================================================================
   知微 · 界面
   ----------------------------------------------------------------------------
   一个文件、零依赖、零构建。理由与后端把资源编译进二进制是同一条：这是跑在
   别人机器上的机器人的界面，不该指望任何一台 CDN 活着，也不该为它引入一套
   打包链。全篇只做四件事——取数据、拼字符串、按 hash 换页、把状态摆对。

   五处纪律：
   - 所有插值一律走 esc()。页面上的字有一半来自配置、日志与人名，其中任何
     一处漏转义都是一个注入点；
   - 不自己造状态。开关、配置、日志都以后端为准，改完重新拉一次，不猜结果。
     唯一的例外是日志缓冲——它只增不改，重画时不能把已经到过的行丢掉；
   - 不往 DOM 里塞样式字面量。视觉值只在 app.css 里，这里只挑类名。唯一例外
     是涟漪的圆心与半径：那两个数由按下的位置决定，算不出别的写法，且只写
     位置与大小，不写颜色；
   - 换页一律走链接（`<a href="#/…">`）与 hashchange。上一版把点击委派挂在
     `#view` 上，而底部导航是它的兄弟节点，于是整条导航点不动；改成链接之后
     即使这段脚本没跑起来，导航照样能换页；
   - 手机上要一直流畅。日志限频合批，标签页切到后台就不动 DOM，动画只碰
     transform 与 opacity，界面里没有模糊与大面积重绘。

   段落：§1 图标 · §2 小工具 · §3 口令 · §4 网络 · §5 反馈与询问 · §6 主题与版式 ·
   §7 外壳与导航 · §8 路由 · §9 各页 · §10 事件 · §11 启动。
   ========================================================================== */

(() => {
  "use strict";

  const NAME = "知微";
  const TOKEN_KEY = "zhiyan.token";
  /** 一行的请求上限。超过它当作「这台机器正忙」，不再让页面停在骨架上。 */
  const REQUEST_TIMEOUT = 20000;

  /* ==================== §1 图标 ==================== */
  /* 一套线性图标，24×24，只用 currentColor，不带填充。 */
  const wrap = (body) =>
    `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" ` +
    `stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

  const ICONS = {
    overview: wrap(`<circle cx="12" cy="12" r="8.2"/><path d="M12 12l3.6-3.6"/>`),
    plugins: wrap(
      `<rect x="3.6" y="3.6" width="7" height="7" rx="2"/>` +
        `<rect x="13.4" y="3.6" width="7" height="7" rx="2"/>` +
        `<rect x="3.6" y="13.4" width="7" height="7" rx="2"/>` +
        `<path d="M16.9 13.6v6.8M13.5 17h6.8"/>`
    ),
    ambient: wrap(
      `<path d="M4 7.2A3.2 3.2 0 017.2 4h9.6A3.2 3.2 0 0120 7.2v6.6a3.2 3.2 0 01-3.2 3.2H9.4L4 21z"/>`
    ),
    logs: wrap(`<path d="M4 6.5h16M4 12h16M4 17.5h9"/>`),
    command: wrap(
      `<rect x="3" y="4.5" width="18" height="15" rx="3"/>` +
        `<path d="M7.5 10l2.6 2-2.6 2M12.8 14h3.7"/>`
    ),
    refresh: wrap(`<path d="M20 12a8 8 0 11-2.4-5.7"/><path d="M20 4.5V10h-5.5"/>`),
    search: wrap(`<circle cx="11" cy="11" r="6.4"/><path d="M15.8 15.8L20 20"/>`),
    chevron: wrap(`<path d="M9.5 5.5l7 6.5-7 6.5"/>`),
    back: wrap(`<path d="M14.5 5.5l-7 6.5 7 6.5"/>`),
    play: wrap(`<path d="M7.5 4.8l11 7.2-11 7.2z"/>`),
    save: wrap(
      `<path d="M5 4.5h11l3.5 3.5v11.5H5z"/><path d="M8.5 4.5v5h6.5v-5"/>` +
        `<path d="M8.5 19.5v-5h7v5"/>`
    ),
    gear: wrap(
      `<circle cx="12" cy="12" r="3.1"/>` +
        `<path d="M19.4 14.4a1.7 1.7 0 00.34 1.87l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.7 1.7 0 00-1.87-.34 1.7 1.7 0 00-1.03 1.55v.17a2 2 0 11-4 0v-.09a1.7 1.7 0 00-1.11-1.55 1.7 1.7 0 00-1.87.34l-.06.06a2 2 0 11-2.83-2.83l.06-.06a1.7 1.7 0 00.34-1.87 1.7 1.7 0 00-1.55-1.03h-.17a2 2 0 110-4h.09A1.7 1.7 0 005.5 8.36a1.7 1.7 0 00-.34-1.87l-.06-.06a2 2 0 112.83-2.83l.06.06a1.7 1.7 0 001.87.34H10a1.7 1.7 0 001.03-1.55v-.17a2 2 0 114 0v.09a1.7 1.7 0 001.03 1.55 1.7 1.7 0 001.87-.34l.06-.06a2 2 0 112.83 2.83l-.06.06a1.7 1.7 0 00-.34 1.87V10a1.7 1.7 0 001.55 1.03h.17a2 2 0 110 4h-.09a1.7 1.7 0 00-1.55 1.03z"/>`
    ),
    plus: wrap(`<path d="M12 5.5v13M5.5 12h13"/>`),
    trash: wrap(
      `<path d="M4.5 6.5h15M9.5 6.5V5a1.5 1.5 0 011.5-1.5h2A1.5 1.5 0 0114.5 5v1.5"/>` +
        `<path d="M6.5 6.5l.9 12a1.6 1.6 0 001.6 1.5h6a1.6 1.6 0 001.6-1.5l.9-12"/>`
    ),
    install: wrap(
      `<rect x="6" y="2.5" width="12" height="19" rx="3"/>` +
        `<path d="M12 7.5v6M9.4 11.1L12 13.7l2.6-2.6"/>`
    ),
    latest: wrap(`<path d="M12 5.5v13M6.4 12.9L12 18.5l5.6-5.6"/>`),
    close: wrap(`<path d="M6 6l12 12M18 6L6 18"/>`),
    copy: wrap(
      `<rect x="8.5" y="8.5" width="11.5" height="11.5" rx="2.5"/>` +
        `<path d="M15.5 5.5A2.5 2.5 0 0013 4H6.5A2.5 2.5 0 004 6.5V13a2.5 2.5 0 001.5 2.3"/>`
    ),
  };

  /* ==================== §2 小工具 ==================== */

  const $ = (selector, root = document) => root.querySelector(selector);

  const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]);

  const num = (value) => Number(value ?? 0).toLocaleString("zh-CN");

  /** 时长说成人话：界面上要读得下去，不摆秒数。 */
  function span(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    const day = Math.floor(total / 86400);
    const hour = Math.floor((total % 86400) / 3600);
    const minute = Math.floor((total % 3600) / 60);
    if (day > 0) return `${day} 天 ${hour} 小时`;
    if (hour > 0) return `${hour} 小时 ${minute} 分`;
    if (minute > 0) return `${minute} 分 ${total % 60} 秒`;
    return `${total} 秒`;
  }

  /** 「多久以前」。搭话的记忆与表情包库里那些时间戳都用它。 */
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

  /** 一次请求的节拍。整屏只有一处会长这样，别在别处再写一遍。 */
  const empty = (text) =>
    `<div class="empty"><div class="empty-icon" aria-hidden="true">${ICONS.logs}</div>
     <div class="empty-text">${esc(text)}</div></div>`;

  const skeleton = (rows = 3) =>
    `<div class="card">${'<div class="skeleton skeleton-lg"></div>'.repeat(rows)}</div>`;

  const reading = (key, value, unit = "") =>
    `<div class="reading"><span class="reading-key">${esc(key)}</span>
     <span class="reading-value">${esc(value)}${
       unit ? `<span class="reading-unit">${esc(unit)}</span>` : ""
     }</span></div>`;

  const pageHead = (title, note = "") =>
    `<header class="page-head">
      <h1 class="page-title" tabindex="-1">${esc(title)}</h1>
      ${note ? `<p class="page-note">${esc(note)}</p>` : ""}
    </header>`;

  /* ==================== §3 口令 ==================== */

  const store = {
    get() {
      try {
        return localStorage.getItem(TOKEN_KEY) || "";
      } catch {
        return "";
      }
    },
    set(token) {
      try {
        localStorage.setItem(TOKEN_KEY, token);
      } catch {
        /* 隐私模式下存不住，这一次会话照常能用 */
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

  let token = new URLSearchParams(location.search).get("t") || store.get();
  if (token) store.set(token);

  /* ==================== §4 网络 ==================== */

  async function api(path, options = {}) {
    const init = {
      method: options.method || "GET",
      headers: { "x-zhiyan-token": token },
    };
    if (options.body !== undefined) {
      init.headers["content-type"] = "application/json";
      init.body = JSON.stringify(options.body);
    }
    const abort = new AbortController();
    const timer = setTimeout(() => abort.abort(), REQUEST_TIMEOUT);
    init.signal = abort.signal;
    let response;
    try {
      response = await fetch(`/api${path}`, init);
    } catch (error) {
      throw abort.signal.aborted
        ? new Error("这一请求 20 秒没有回应；这台机器可能正忙，过一会儿再试")
        : new Error("连不上控制台；它可能刚被关掉，或者这一页是离线的旧页面");
    } finally {
      clearTimeout(timer);
    }
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(payload.error || `服务返回 ${response.status}`);
      error.status = response.status;
      throw error;
    }
    return payload;
  }

  /* ==================== §5 反馈与询问 ==================== */

  const snackHost = () => $("#snack");
  let snackTimer = 0;

  /** 一条反馈。`busy` 不自动消失，等下一次调用把它换掉。 */
  function snack(text, kind = "good") {
    clearTimeout(snackTimer);
    const busy = kind === "busy";
    snackHost().innerHTML = `
      <div class="snack snack-${busy ? "good" : kind}">
        ${busy ? `<span class="indicator indicator-inline"></span>` : ""}
        <span class="snack-text">${esc(text)}</span>
        <button class="snack-close" type="button" data-snack-close aria-label="收起">${ICONS.close}</button>
      </div>`;
    if (busy) return;
    // 报错多留一会儿：一句话要读完，还要来得及照它做。
    snackTimer = setTimeout(() => (snackHost().innerHTML = ""), kind === "bad" ? 8000 : 3600);
  }

  function fail(error) {
    if (error && error.status === 401) {
      store.clear();
      paintLock("口令不对，或者它已经换过了");
      return;
    }
    snack(error && error.message ? error.message : String(error), "bad");
  }

  /** 问一句再动手。用原生 dialog：Esc、焦点陷阱、返回键都由浏览器给。 */
  function ask({ title, body, confirm = "继续", danger = false }) {
    const dialog = $("#dialog");
    if (!dialog || typeof dialog.showModal !== "function") {
      return Promise.resolve(window.confirm(`${title}\n\n${body}`));
    }
    dialog.innerHTML = `
      <div class="dialog-title" id="dialog-title">${esc(title)}</div>
      <p class="dialog-body" id="dialog-body">${esc(body)}</p>
      <div class="dialog-actions">
        <button class="btn" type="button" data-answer="no">取消</button>
        <button class="btn ${danger ? "btn-filled btn-danger" : "btn-filled"}"
                type="button" data-answer="yes">${esc(confirm)}</button>
      </div>`;
    return new Promise((resolve) => {
      const pick = (event) => {
        const answer = event.target.closest("[data-answer]");
        if (!answer) return;
        dialog.close();
        resolve(answer.dataset.answer === "yes");
      };
      dialog.addEventListener("click", pick);
      // Esc 关掉时也算「不动手」：close 事件里统一收口，避免两个 resolve。
      dialog.addEventListener("close", () => {
        dialog.removeEventListener("click", pick);
        resolve(false);
      }, { once: true });
      dialog.showModal();
    });
  }

  /* ==================== §6 主题与版式 ==================== */

  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)");
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  const narrow = window.matchMedia("(min-width: 600px)");
  const wide = window.matchMedia("(min-width: 840px)");

  function applyTheme() {
    document.body.classList.toggle("dark", prefersDark.matches);
  }

  /** 三档版式：紧凑（底栏）· 中等（导航轨）· 宽（抽屉）。判据与 app.css 一致。 */
  function layout() {
    return wide.matches ? "expanded" : narrow.matches ? "medium" : "compact";
  }

  function applyLayout() {
    const name = layout();
    document.body.classList.remove("layout-compact", "layout-medium", "layout-expanded");
    document.body.classList.add(`layout-${name}`);
    return name;
  }

  /** 装到桌面之后浏览器不再给地址栏，界面也该知道自己不在标签页里了。 */
  const standalone = () =>
    window.matchMedia("(display-mode: standalone)").matches ||
    window.matchMedia("(display-mode: fullscreen)").matches ||
    window.navigator.standalone === true;

  /* ==================== §7 外壳与导航 ==================== */

  const PAGES = [
    { id: "overview", label: "总览", title: "总览" },
    { id: "plugins", label: "插件", title: "插件" },
    { id: "ambient", label: "搭话", title: "搭话" },
    { id: "logs", label: "日志", title: "日志" },
    { id: "command", label: "命令", title: "命令" },
  ];

  const NAV_HINT = "知微 · 工作台";

  /** 导航只搭一次：每次重画都会把选中态的过渡打断，看着像闪。 */
  function buildNav() {
    $("#nav").innerHTML =
      `<span class="nav-hint">${esc(NAV_HINT)}</span>` +
      PAGES.map(
        (page) => `
      <a class="nav-item" href="#/${page.id}" data-nav="${page.id}">
        <span class="nav-indicator">${ICONS[page.id]}</span>
        <span class="nav-label">${esc(page.label)}</span>
      </a>`
      ).join("");
  }

  function markNav(active) {
    for (const item of document.querySelectorAll("#nav [data-nav]")) {
      if (item.dataset.nav === active) item.setAttribute("aria-current", "page");
      else item.removeAttribute("aria-current");
    }
  }

  let pinObserver = null;

  /** 页头滚到顶栏底下之后，紧凑屏的顶栏把品牌换成页名。 */
  function watchPageHead() {
    if (pinObserver) pinObserver.disconnect();
    const bar = $(".bar");
    const head = $(".page-head");
    if (!bar || !head || !("IntersectionObserver" in window)) return;
    bar.removeAttribute("data-pinned");
    pinObserver = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) bar.removeAttribute("data-pinned");
        else bar.setAttribute("data-pinned", "");
      },
      { rootMargin: `-${bar.offsetHeight + 1}px 0px 0px 0px`, threshold: 0 }
    );
    pinObserver.observe(head);
  }

  /* ==================== §8 路由 ==================== */

  function currentRoute() {
    const hash = location.hash.replace(/^#\/?/, "");
    if (!hash) return { page: "overview", arg: null, detail: false };
    const [head, ...rest] = hash.split("/");
    if (head === "plugins" && rest.length) {
      try {
        return { page: "plugins", arg: decodeURIComponent(rest.join("/")), detail: true };
      } catch { return { page: "plugins", arg: null, detail: false }; }
    }
    if (head === "settings") return { page: "settings", arg: null, detail: false };
    return PAGES.some((page) => page.id === head)
      ? { page: head, arg: null, detail: false }
      : { page: "overview", arg: null, detail: false };
  }

  /** 打开某个路由。用链接或这里都行——两者都只改 hash，剩下的交给 hashchange。 */
  function go(page, arg) {
    const hash = arg ? `#/${page}/${encodeURIComponent(arg)}` : `#/${page}`;
    if (location.hash === hash) render();
    else location.hash = hash;
  }

  let renderSeq = 0;
  let lastPage = "";
  const scrollMemory = new Map();

  async function render() {
    if (!token) return;
    const route = currentRoute();
    if (route.page !== "logs") stopLogs();
    const navKey = route.detail ? "plugins" : route.page;
    const pageKey = route.detail ? `plugins/${route.arg}` : route.page;
    markNav(navKey);

    const title = route.page === "settings" ? "接入与全局" : route.detail ? route.arg : (PAGES.find((p) => p.id === route.page) || PAGES[0]).title;
    $(".bar-title").textContent = title;
    document.title = route.detail ? `${route.arg} · ${NAME}` : `${title} · ${NAME}`;

    const seq = ++renderSeq;
    const view = $("#view");
    // 换页时把上一页的滚动位置记住，回头再进来不至于从头翻。
    if (lastPage && lastPage !== pageKey) scrollMemory.set(lastPage, window.scrollY);
    logState.mounted = false;
    detailSeq++;
    view.setAttribute("aria-busy", "true");
    view.innerHTML = skeleton(route.detail ? 2 : 1);

    let html;
    try {
      html = await paintRoute(route);
    } catch (error) {
      if (seq !== renderSeq) return;
      if (error && error.status === 401) {
        paintLock();
        return;
      }
      view.removeAttribute("aria-busy");
      view.innerHTML =
        pageHead("读不出来") +
        empty(
          error && error.status === 503
            ? error.message
            : `这一页没能读出来：${error && error.message ? error.message : "原因不明"}`
        );
      return;
    }
    if (seq !== renderSeq) return;

    const changed = lastPage !== pageKey;
    lastPage = pageKey;
    view.innerHTML = html;
    view.dataset.page = route.page;
    view.removeAttribute("aria-busy");
    if (route.detail) {
      const display = layout() === "expanded"
        ? pluginView.payload?.plugins.find((p) => p.name === route.arg)?.display
        : $(".page-title")?.textContent;
      $(".bar-title").textContent = display || route.arg;
      document.title = `${display || route.arg} · ${NAME}`;
    }
    if (changed) {
      const remembered = scrollMemory.get(pageKey) || 0;
      window.scrollTo({ top: remembered, behavior: "auto" });
      if (!reduced.matches) {
        view.removeAttribute("data-enter");
        void view.offsetWidth;
        view.dataset.enter = "";
      }
    }
    watchPageHead();
    mountRoute(route);
    if (changed && seq > 1) $(".page-title")?.focus({ preventScroll: true });
  }

  async function paintRoute(route) {
    // 插件页在宽屏上是「列表 + 详情」并排，窄屏上详情另占一页。
    if (route.detail) {
      return layout() === "expanded" ? paintPluginsSplit(route.arg) : paintPluginDetail(route.arg);
    }
    switch (route.page) {
      case "plugins":
        return layout() === "expanded" ? paintPluginsSplit("") : paintPlugins();
      case "ambient":
        return paintAmbient();
      case "logs":
        return paintLogs();
      case "command":
        return paintCommand();
      case "settings":
        return paintSettings();
      default:
        return paintOverview();
    }
  }

  function mountRoute(route) {
    if (route.detail) return;
    if (route.page === "overview") mountOverviewLog();
    if (route.page === "logs") mountLogs();
  }

  /* ==================== §9 各页 ==================== */

  /* ---- 总览 ---- */

  async function paintOverview() {
    const data = await api("/overview");
    const bots = data.bots.length
      ? data.bots
          .map(
            (bot) => `
        <div class="row row-plain">
          <div class="row-icon">${esc((bot.name || bot.nick || "?").slice(0, 1))}</div>
          <div class="row-body">
            <span class="row-title">${esc(bot.name || bot.nick || bot.id || "未取得账号")}
              <span class="key">${esc(bot.adapter)}/${esc(bot.platform)}</span>
            </span>
            <span class="row-sub">${
              bot.id ? `已连上，账号 ${esc(bot.id)}` : "已连接实现端，账号还没认出来"
            }</span>
          </div>
        </div>`
          )
          .join("")
      : empty("还没有连接；启动日志里找「启动适配器」那几行");

    return `
      ${pageHead("总览", "运行、连接与消息，一眼了解。") }
      <div class="overview-grid">
      <section class="hero">
        <div class="hero-top"><span class="hero-eyebrow">${esc(NAME)} · ${esc(data.app.version)}</span>
          <span class="badge badge-on">运行中</span></div>
        <span class="hero-eyebrow">本次运行时长</span>
        <div class="hero-figure">
          <span class="hero-number">${esc(span(data.app.uptime))}</span>
        </div>
        <span class="hero-note">启动于 ${esc(data.app.started)}</span>
        <div class="hero-actions">
          <a class="btn btn-filled" href="#/logs">${ICONS.logs}查看日志</a>
          <a class="btn btn-tonal" href="#/plugins">${ICONS.plugins}管理插件</a>
        </div>
      </section>

      <section class="card overview-metrics">
        <div class="section-title">消息与插件<span class="count">打开时更新</span></div>
        <div class="readings">
          ${reading("今日消息", num(data.messages.today), "条")}
          ${reading("今日发言人", num(data.messages.people), "位")}
          ${reading("近 7 天消息", num(data.messages.week), "条")}
          ${reading(
            "插件",
            `${data.plugins.on}/${data.plugins.total}`,
            data.plugins.pending ? ` · ${data.plugins.pending} 个待重启` : ""
          )}
        </div>
      </section>

      <section class="card card-notched overview-connections">
        <div class="section-title">连接<span class="count">${data.bots.length} 条</span></div>
        <div class="list">${bots}</div>
      </section>

      <section class="card overview-recent">
        <div class="section-title">最近日志<span class="count">最近 6 行</span></div>
        <div class="log log-short" id="overview-log"></div>
        <div class="actions">
          <a class="btn btn-tonal" href="#/logs">看全部</a>
        </div>
      </section>
      </div>`;
  }

  /** 总览的「最近日志」：一屏六行，跟着走，不订阅日志流。 */
  const OVERVIEW_LINES = 6;
  const OVERVIEW_EVERY = 6000;

  async function mountOverviewLog() {
    const box = $("#overview-log");
    if (!box) return;
    await refreshOverviewLog();
    // 这一页是落地页，那六行不该停在打开那一刻。拉的是内存里的环形缓冲，
    // 不碰数据库也不碰平台；六秒一次，一次六个 JSON 对象。
    clearInterval(logState.overview);
    logState.overview = setInterval(() => {
      if (document.hidden || !$("#overview-log")) {
        clearInterval(logState.overview);
        logState.overview = 0;
        return;
      }
      refreshOverviewLog();
    }, OVERVIEW_EVERY);
  }

  async function refreshOverviewLog() {
    const box = $("#overview-log");
    if (!box) return;
    try {
      const data = await api(`/logs?limit=${OVERVIEW_LINES}`);
      const lines = (data.lines || []).slice(-OVERVIEW_LINES);
      const html = lines.length
        ? lines.map(logLine).join("")
        : `<div class="log-line log-debg">这里还是空的</div>`;
      // 一模一样就不动 DOM：每六秒重画一次会让滚动位置与选区一起跳。
      if (box.dataset.digest === html) return;
      box.dataset.digest = html;
      box.innerHTML = html;
      box.scrollTop = box.scrollHeight;
    } catch {
      if (!box.children.length) box.textContent = "日志暂时无法读取，可刷新重试";
    }
  }

  /* ---- 插件 ---- */

  const pluginView = { payload: null, text: "", section: "", selected: "" };

  function pluginRows() {
    const data = pluginView.payload;
    const rows = data.plugins
      .filter((plugin) => {
        if (pluginView.section && plugin.section !== pluginView.section) return false;
        const text = pluginView.text.trim().toLowerCase();
        if (!text) return true;
        return (
          plugin.name.includes(text) ||
          plugin.display.toLowerCase().includes(text) ||
          plugin.summary.toLowerCase().includes(text)
        );
      })
      .map(
        (plugin) => `
        <div class="row" data-plugin="${esc(plugin.name)}"
             ${plugin.name === pluginView.selected ? 'aria-selected="true"' : ""}>
          <a class="row-hit" href="#/plugins/${encodeURIComponent(plugin.name)}"
             aria-label="打开 ${esc(plugin.display)}"></a>
          <div class="row-body">
            <span class="row-title">${esc(plugin.display)}
              <span class="key">${esc(plugin.name)}</span>
              ${plugin.pending ? badge(plugin) : ""}
            </span>
            <span class="row-sub">${esc(plugin.summary)}</span>
          </div>
          <div class="row-tail">
            <button class="switch" type="button" role="switch" data-toggle="${esc(plugin.name)}"
                    aria-checked="${plugin.on}" aria-label="启用或停用${esc(plugin.display)}"
                    ${plugin.name === "ctl" ? "disabled" : ""}></button>
            <span class="chevron">${ICONS.chevron}</span>
          </div>
        </div>`
      )
      .join("");
    return `<div class="list">${rows || empty("没有符合条件的插件")}</div>`;
  }

  function pluginChips() {
    const data = pluginView.payload;
    return [
      `<button class="chip" type="button" data-section=""
         aria-pressed="${pluginView.section === ""}">全部 ${data.plugins.length}</button>`,
      ...data.sections
        .map((section) => {
          const count = data.plugins.filter((plugin) => plugin.section === section.code).length;
          if (!count) return "";
          return `<button class="chip" type="button" data-section="${esc(section.code)}"
            aria-pressed="${pluginView.section === section.code}">${esc(section.name)} ${count}</button>`;
        })
        .filter(Boolean),
    ].join("");
  }

  const pluginSearch = () => `
    <div class="search">
      ${ICONS.search}
      <input id="plugin-search" type="search" placeholder="按名字或说明找"
             value="${esc(pluginView.text)}" aria-label="搜索插件">
    </div>`;

  /** 只重画列表与筛选项：敲一个字就整页重拉一次太浪费，也会把光标顶掉。 */
  function repaintPlugins() {
    const list = $("#plugin-list");
    if (list) list.innerHTML = pluginRows();
    const chips = $("#plugin-chips");
    if (chips) chips.innerHTML = pluginChips();
  }

  async function paintPlugins() {
    pluginView.payload = await api("/plugins");
    pluginView.selected = "";
    return `
      ${pageHead("插件", "开关就地改，改完立刻生效；点一行进去改它的配置。")}
      ${pluginSearch()}
      <div class="chips" id="plugin-chips">${pluginChips()}</div>
      <section class="card" id="plugin-list">${pluginRows()}</section>`;
  }

  /** 宽屏：列表与详情并排，选一个就地展开，不必来回翻页。 */
  async function paintPluginsSplit(name) {
    pluginView.payload = await api("/plugins");
    pluginView.selected = name || "";
    const detail = name
      ? pluginDetailHtml(await fetchPlugin(name))
      : `<section class="card">${empty("左边挑一个插件，它的配置与指令会在这儿展开")}</section>`;
    return `
      ${pageHead("插件", "开关就地改，改完立刻生效；右边是选中那个的配置。")}
      <div class="panes" data-split>
        <div class="pane">
          ${pluginSearch()}
          <div class="chips" id="plugin-chips">${pluginChips()}</div>
          <section class="card" id="plugin-list">${pluginRows()}</section>
        </div>
        <div class="pane" id="plugin-detail">${detail}</div>
      </div>`;
  }

  function badge(plugin) {
    if (plugin.pending) return `<span class="badge badge-pending">待重启</span>`;
    return plugin.on
      ? `<span class="badge badge-on">已启用</span>`
      : `<span class="badge badge-off">已停用</span>`;
  }

  /* ---- 插件详情 ---- */

  const fetchPlugin = (name) => api(`/plugins/${encodeURIComponent(name)}`);

  async function paintPluginDetail(name) {
    const plugin = await fetchPlugin(name);
    pluginView.selected = name;
    // 页头已经写着插件名了，卡里不再重复一遍
    return `${pageHead(plugin.display)}${pluginDetailHtml(plugin, { back: true, named: false })}`;
  }

  function pluginDetailHtml(plugin, { back = false, named = true } = {}) {
    const rows = Object.entries(plugin.config)
      .map(([key, value]) => renderNode(key, key, value))
      .join("");
    const differences = plugin.diff.length
      ? `<div class="code">${esc(plugin.diff.join("\n"))}</div>`
      : `<p class="note">与默认值一致。</p>`;
    const commands = plugin.commands.length
      ? plugin.commands
          .map(
            (command) => `
            <button class="row row-plain" type="button" data-copy="${esc(command.cmd)}"
                    title="点击复制">
              <div class="row-body">
                <span class="row-title"><span class="key">${esc(command.cmd)}</span></span>
                <span class="row-sub">${esc(command.note)}</span>
              </div>
              <div class="row-tail"><span class="chevron">${ICONS.copy}</span></div>
            </button>`
          )
          .join("")
      : `<p class="note">这个插件没有指令，它在后台按排期自己工作。</p>`;

    return `
      ${back ? `<a class="btn btn-tonal back" href="#/plugins">${ICONS.back}回插件列表</a>` : ""}
      <section class="card">
        <div class="section-title">${named ? esc(plugin.display) : ""}
          <span class="key">${esc(plugin.name)}</span>${badge(plugin)}</div>
        <p class="note">${esc(plugin.summary)}</p>
        <div class="row row-plain">
          <div class="row-body">
            <span class="row-title">开关</span>
            <span class="row-sub">${esc(plugin.effect)}</span>
          </div>
          <button class="switch" type="button" role="switch" data-toggle="${esc(plugin.name)}"
                  aria-checked="${plugin.on}" aria-label="启用或停用"
                  ${plugin.name === "ctl" ? "disabled" : ""}></button>
        </div>
      </section>

      <div class="split">
        <section class="card">
          <div class="section-title">配置<span class="count">改了立刻生效</span></div>
          <div class="panel" data-config-plugin="${esc(plugin.name)}">${rows}</div>
          <div class="actions">
            <button class="btn btn-outline" type="button" data-reset="${esc(plugin.name)}">
              恢复默认参数（保留开关）</button>
          </div>
        </section>
        <section class="card">
          <div class="section-title">和默认差在哪</div>
          ${differences}
          <div class="section-title">指令
            <span class="count">${plugin.commands.length} 条 · 点一条复制</span></div>
          <div class="list">${commands}</div>
        </section>
      </div>`;
  }

  /** 一个配置节点：表与对象数组往下拆，叶子就地渲染成能改的控件。 */
  function renderNode(path, key, value) {
    if (Array.isArray(value) && value.every((item) => item === null || typeof item !== "object")) {
      return kv(
        path,
        key,
        `<input class="input input-mono" data-path="${esc(path)}" data-kind="list"
          value="${esc(value.join(", "))}" spellcheck="false" aria-label="${esc(key)}">`
      );
    }
    if (value !== null && typeof value === "object") {
      const inner = Array.isArray(value)
        ? value.map((item, index) => renderNode(`${path}.${index}`, `[${index}]`, item)).join("")
        : Object.entries(value)
            .map(([child, item]) => renderNode(`${path}.${child}`, child, item))
            .join("");
      return kv(path, key, `<div class="subtable">${inner}</div>`);
    }
    if (typeof value === "boolean") {
      return kv(
        path,
        key,
        `<button class="switch" type="button" role="switch" data-path="${esc(path)}"
           data-kind="bool" aria-checked="${value}" aria-label="${esc(key)}"></button>`
      );
    }
    // 长文本（提示词、人格那类）给一块多行的地方，单行输入框里换行会被吃掉。
    const long = typeof value === "string" && (value.includes("\n") || value.length > 120);
    if (long) {
      return `<div class="kv kv-wide">
        <span class="kv-key" title="${esc(path)}">${esc(key)}</span>
        <textarea class="area area-short" data-path="${esc(path)}" data-kind="string"
                  spellcheck="false" aria-label="${esc(key)}">${esc(value)}</textarea></div>`;
    }
    const kind = typeof value === "number" ? "number" : "string";
    return kv(
      path,
      key,
      `<input class="input input-mono" data-path="${esc(path)}" data-kind="${kind}"
        value="${esc(value ?? "")}" spellcheck="false" aria-label="${esc(key)}">`
    );
  }

  const kv = (path, key, control) =>
    `<div class="kv"><span class="kv-key" title="${esc(path)}">${esc(key)}</span>
     <span class="kv-value">${control}</span></div>`;

  /** 输入框里的字变回配置值。逗号分开的一行当数组，数字按数字写回去。 */
  function parseValue(kind, raw) {
    if (kind === "bool") return Boolean(raw);
    if (kind === "number") {
      const value = Number(raw);
      if (!Number.isFinite(value)) throw new Error("这一项要填数字");
      return value;
    }
    if (kind !== "list") return raw;
    const text = raw.trim();
    if (!text) return [];
    return text.split(",").map((part) => {
      const item = part.trim();
      if (/^-?\d+$/.test(item)) return Number.parseInt(item, 10);
      if (/^-?\d*\.\d+$/.test(item)) return Number.parseFloat(item);
      return item.replace(/^["']|["']$/g, "");
    });
  }

  /* ---- 搭话 ---- */

  async function paintAmbient() {
    const data = await api("/ambient");
    if (!data.ready) return empty("搭话插件的目录还没建起来，先让机器人跑一轮");

    const groups = data.memory.length
      ? data.memory.map(renderGroupMemory).join("")
      : empty("还没记住任何群");
    const gallery = data.stickers.length
      ? data.stickers.map(renderSticker).join("")
      : empty("库里还没有东西；它在群里收下一张就会存一张");

    return `
      ${pageHead("搭话", "人格、档案、记忆与表情包库都在这儿。")}

      <section class="card">
        <div class="section-title">它在群里像谁
          <span class="count">改完下一轮生效</span></div>
        <p class="note">人格是它在群里说话的样子，档案是它知道自己是谁。
        两份都直接写进运行目录，旧的那份存成同名的 backup；
        线上那份与仓库里那份是两回事，改这里不动仓库。</p>
        <label class="note" for="persona">人格 persona.md</label>
        <textarea class="area" id="persona" spellcheck="false">${esc(data.persona)}</textarea>
        <label class="note" for="self">档案 self.md</label>
        <textarea class="area area-short" id="self" spellcheck="false">${esc(data.self)}</textarea>
        <div class="actions">
          <button class="btn btn-filled" type="button" data-source="persona">
            ${ICONS.save}保存人格</button>
          <button class="btn btn-outline" type="button" data-source="self">
            ${ICONS.save}保存档案</button>
        </div>
      </section>

      <section class="card card-notched">
        <div class="section-title">记得什么
          <span class="count">${data.memory.length} 个群</span></div>
        <div class="list">${groups}</div>
      </section>

      <section class="card">
        <div class="section-title">表情包库
          <span class="count">${data.stickers.length} 张</span></div>
        <div class="gallery">${gallery}</div>
      </section>`;
  }

  function renderSticker(sticker) {
    return `
      <div class="tile">
        ${
          sticker.image
            ? `<img class="tile-img" loading="lazy" decoding="async" alt="${esc(sticker.label)}"
                 src="/api/ambient/sticker/${sticker.id}?t=${encodeURIComponent(token)}">`
            : `<div class="tile-img tile-blank">商城表情<br>只存了参数</div>`
        }
        <span class="tile-label">${esc(sticker.label || "没起名的表情包")}</span>
        <span class="note">#${sticker.id} · 用过 ${sticker.uses} 次 · ${esc(
          ago(sticker.added_at)
        )}</span>
      </div>`;
  }

  function renderGroupMemory(group) {
    const noted = group.people.filter((person) => person.note || person.address);
    const others = group.people.length - noted.length;
    const people = noted
      .map(
        (person) => `
        <div class="row row-plain">
          <div class="row-body">
            <span class="row-title">${esc(person.name || person.id)}
              ${person.address ? `<span class="key">叫「${esc(person.address)}」</span>` : ""}
            </span>
            <span class="row-sub">${esc(person.note || "有称呼，没写印象")}</span>
          </div>
          <div class="row-tail">
            <span class="note">${num(person.messages)} 条 · ${esc(ago(person.last_seen))}</span>
          </div>
        </div>`
      )
      .join("");
    const notes = group.notes
      .map(
        (note) => `
        <div class="row row-plain">
          <div class="row-body"><span class="row-sub">${esc(note.text)}</span></div>
          <div class="row-tail"><span class="note">${esc(ago(note.at))}</span></div>
        </div>`
      )
      .join("");

    return `
      <div class="inset">
        <div class="section-title">群 ${esc(group.group)}
          <span class="count">${group.people.length} 人 · ${group.notes.length} 条旧事</span>
        </div>
        <p class="note">${
          noted.length
            ? `有印象的 ${noted.length} 位${others > 0 ? `，另有 ${others} 位只记得露过面` : ""}`
            : "这个群里它还谁都没写上印象"
        }</p>
        <div class="list">${people}${notes}</div>
      </div>`;
  }

  /* ---- 日志 ----
     跟随是阅读意图，不从每一次布局产生的 scroll 事件反推。
     真实向上滚动暂停；回到底部恢复。面板尺寸变化只在跟随时重新贴底。
     暂停保留 DOM、选区和位置，后台关闭连接，回来以快照补齐有界缓冲。
     筛选必须先于显示队列截断，避免密集 INFO 淹没最新一条 WARN。
  */
  const DOM_LINES = 80;
  const LOG_BATCH_MS = 100;
  const LOG_INSERT_MAX = 40;
  /** 贴底的容差。差这么点算贴着，再远一点才算用户翻上去了。 */
  const STICK_SLACK = 24;
  /** 断了之后的退避：2、4、8、16 秒，封顶 30 秒。 */
  const LOG_RETRY_BASE = 2000;
  const LOG_RETRY_MAX = 30000;

  const logState = {
    lines: [], level: "", text: "", follow: true, source: null,
    limit: 2000, queue: [], frame: 0, mounted: false, status: "正在连接…",
    fresh: 0, failures: 0, retry: 0, overview: 0, resize: null, scrollTop: 0,
  };

  function logLine(entry) {
    const level = String(entry.level || "INFO").toLowerCase();
    return `<div class="log-line log-${esc(level)}">
      <span class="log-at">${esc(entry.at)}</span>
      <span class="log-level">${esc(entry.level || "INFO")}</span>
      <span class="log-target">${esc(entry.target)}</span>
      <span class="log-text">${esc(entry.text)}</span></div>`;
  }

  function logMatches(entry) {
    if (logState.level && entry.level !== logState.level) return false;
    const text = logState.text.trim().toLowerCase();
    if (!text) return true;
    return String(entry.text).toLowerCase().includes(text) ||
      String(entry.target).toLowerCase().includes(text);
  }

  /** 面板贴在底部。差值可能因折行而变化，所以只用于判断，不用于定位。 */
  function logStuck(box) {
    return box.scrollHeight - box.scrollTop - box.clientHeight <= STICK_SLACK;
  }

  /** 贴到底部。读 scrollHeight 会强制一次布局，拿到的就是刚插进去那一行的真实高度。 */
  function logPin(box) {
    box.scrollTop = box.scrollHeight;
    logState.scrollTop = box.scrollTop;
  }

  /** 面板上下两处跟着状态走的东西：暂停按钮上的字，与「回到最新」上的条数。 */
  function syncLogChrome() {
    const countLabel = $("#log-count");
    if (countLabel) countLabel.textContent = `${$("#log-box")?.querySelectorAll(".log-line").length || 0} 行可见 · ${logState.lines.length} 行缓冲`;
    const latest = $("#log-latest");
    if (latest) latest.textContent = logState.lines.length ? `最近收到 ${logState.lines.at(-1).at}` : "等待记录";
    const button = $("#log-follow");
    if (button) {
      button.setAttribute("aria-pressed", String(logState.follow));
      button.textContent = logState.follow ? "跟随最新" : "已暂停";
    }
    const jump = $("#log-jump");
    if (!jump) return;
    jump.classList.toggle("jump-on", !logState.follow);
    const count = logState.fresh ? String(logState.fresh) : "";
    if (jump.dataset.fresh !== count) {
      jump.dataset.fresh = count;
      jump.innerHTML = `${ICONS.latest}<span>回到最新${
        count ? ` · <b>${count}</b> 条` : ""
      }</span>`;
    }
  }

  function paintLog() {
    const box = $("#log-box");
    if (!box || document.hidden) return;
    clearTimeout(logState.frame);
    logState.frame = 0;
    logState.queue = [];
    const shown = logState.lines.filter(logMatches).slice(-DOM_LINES);
    box.innerHTML = shown.length
      ? shown.map(logLine).join("")
      : `<div class="log-empty">没有符合条件的日志</div>`;
    logState.fresh = 0;
    syncLogChrome();
    if (logState.follow) logPin(box);
  }

  function logStatus(text) {
    // 状态记在状态里、由渲染读出来。整屏重画（顶栏刷新、跨断点）时连接还在，
    // 若写死字面量，重画之后就会永远停在「正在连接…」。
    logState.status = text;
    const status = $("#log-status");
    if (status) status.textContent = text;
  }

  /** 跟随开关。恢复时按缓冲重画一次：暂停期间新行只进了缓冲，没进 DOM。 */
  function setFollow(on) {
    logState.scrollTop = $("#log-box")?.scrollTop || 0;
    if (logState.follow !== on) {
      logState.follow = on;
      if (on) logState.fresh = 0;
      syncLogChrome();
    }
    if (on && $("#log-box")) paintLog();
  }

  // 暂停时保留当前 DOM 与选择区域；到来的行只进有界缓冲。
  function pushLogs(entries) {
    logState.lines.push(...entries);
    if (logState.lines.length > logState.limit) {
      logState.lines.splice(0, logState.lines.length - logState.limit);
    }
    if (document.hidden || !logState.mounted || !logState.follow) {
      // 屏上不动的这段时间，只把「回来之后能补上多少」记在按钮上。
      logState.fresh += entries.filter(logMatches).length;
      syncLogChrome();
      return;
    }
    logState.queue.push(...entries.filter(logMatches));
    if (logState.queue.length > DOM_LINES) {
      logState.queue.splice(0, logState.queue.length - DOM_LINES);
    }
    if (!logState.frame) logState.frame = setTimeout(flushLog, LOG_BATCH_MS);
  }

  // 每秒至多十次 DOM 合批，限单批与总节点数；不随消息频率重复排版。
  // 一次只插 LOG_INSERT_MAX 行，剩下的下一拍接着插——突发时单次任务因此有上限，
  // 而屏幕上最终留下的仍是最近 DOM_LINES 行（队列超了丢的是最旧的那几行）。
  function flushLog() {
    logState.frame = 0;
    const box = $("#log-box");
    if (!box || document.hidden || !logState.mounted || !logState.follow) return;
    const fresh = logState.queue.splice(0, LOG_INSERT_MAX).filter(logMatches);
    const rest = logState.queue.length;
    if (fresh.length) {
      $(".log-empty", box)?.remove();
      box.insertAdjacentHTML("beforeend", fresh.map(logLine).join(""));
      while (box.children.length > DOM_LINES) box.firstElementChild.remove();
      logPin(box);
    }
    syncLogChrome();
    if (rest) logState.frame = setTimeout(flushLog, LOG_BATCH_MS);
  }

  function paintLogs() {
    const levels = [["", "全部"], ["INFO", "信息"], ["WARN", "警告"], ["ERRO", "错误"], ["DEBG", "调试"]];
    return `
      ${pageHead("日志", "运行的每一步，都在这里。向上翻阅暂停，回到底部继续跟随。")}
      <section class="log-workspace" aria-label="日志工作区">
      <div class="toolbar log-tools">
        <div class="search">${ICONS.search}
          <input id="log-search" type="search" placeholder="搜索内容或来源"
                 value="${esc(logState.text)}" aria-label="过滤日志">
        </div>
        <div class="seg log-levels" data-connected role="group" aria-label="日志级别">
          ${levels.map(([value, label]) => `<button class="seg-item" type="button" data-level="${value}"
            aria-pressed="${logState.level === value}">${label}</button>`).join("")}
        </div>
        <div class="log-actions">
        <button class="chip" type="button" id="log-follow" aria-pressed="${logState.follow}">
          ${logState.follow ? "跟随最新" : "已暂停"}</button>
        <button class="btn" type="button" id="log-export">导出记录</button>
        <button class="btn btn-icon" type="button" id="log-clear" title="清空这一屏"
                aria-label="清空这一屏">${ICONS.trash}</button>
      </div>
      </div>
      <div class="log-meta"><span id="log-status" role="status">${esc(logState.status)}</span>
        <span id="log-latest">等待记录</span></div>
      <div class="log" id="log-box" tabindex="0" role="region" aria-label="运行日志"></div>
      <button class="btn btn-filled jump ${logState.follow ? "" : "jump-on"}" type="button"
              id="log-jump" data-fresh="${logState.fresh || ""}"><span>回到最新</span></button>
      <div class="log-footer"><span id="log-count"></span><span>显示最近 ${DOM_LINES} 条匹配记录</span></div>
      </section>
      <p class="note">离开或切到后台时暂停接收，返回后补齐最近记录。导出当前筛选的缓冲记录；完整日志用 <span class="key">./bot logs</span> 查看。</p>`;
  }

  function stopLogs() {
    logState.resize?.disconnect();
    logState.resize = null;
    logState.source?.close();
    logState.source = null;
    logState.mounted = false;
    clearTimeout(logState.frame);
    clearTimeout(logState.retry);
    clearInterval(logState.overview);
    logState.frame = 0;
    logState.retry = 0;
    logState.overview = 0;
    logState.failures = 0;
    logState.queue = [];
  }

  /** 建一条新的日志流。事件都在 `source === logState.source` 时才认，避免旧流的尾巴改到新状态。 */
  function openLogs() {
    const source = new EventSource(`/api/logs/stream?t=${encodeURIComponent(token)}`);
    logState.source = source;
    source.addEventListener("open", () => {
      if (source !== logState.source) return;
      logState.failures = 0;
      logStatus("实时连接");
    });
    source.addEventListener("snapshot", (event) => {
      if (source !== logState.source) return;
      try {
        const data = JSON.parse(event.data);
        logState.lines = data.lines.slice(-logState.limit);
        logState.queue = [];
        if (logState.follow || !$("#log-box")?.querySelector(".log-line")) paintLog();
        syncLogChrome();
        if (data.dropped) logStatus(`追不上了，中间跳过 ${data.dropped} 行`);
      } catch { logStatus("记录未能读取，刷新重试"); }
    });
    source.addEventListener("batch", (event) => {
      if (source !== logState.source) return;
      try { pushLogs(JSON.parse(event.data).lines); } catch { /* 忽略损坏的一批 */ }
    });
    source.onmessage = (event) => {
      if (source !== logState.source) return;
      try { pushLogs([JSON.parse(event.data)]); } catch { /* 忽略损坏的一行 */ }
    };
    source.onerror = () => {
      if (source !== logState.source) return;
      // CONNECTING 说明浏览器正在自己重连，等它；CLOSED 才是彻底断了。
      if (source.readyState !== EventSource.CLOSED) {
        logStatus("连接中断，正在重连…");
        return;
      }
      source.close();
      logState.source = null;
      const delay = Math.min(LOG_RETRY_MAX, LOG_RETRY_BASE * 2 ** Math.min(logState.failures, 4));
      logState.failures += 1;
      logStatus(`连接已断开，${Math.round(delay / 1000)} 秒后重试`);
      clearTimeout(logState.retry);
      logState.retry = setTimeout(() => {
        logState.retry = 0;
        if (logState.mounted && !document.hidden) openLogs();
      }, delay);
    };
  }

  function connectLogs() {
    // 连着的、或者正在自动重连的，不另开一条。
    if (logState.source && logState.source.readyState !== EventSource.CLOSED) return;
    openLogs();
  }

  function mountLogs() {
    const box = $("#log-box");
    if (!box) {
      logState.mounted = false;
      return;
    }
    logState.mounted = true;
    // 同一页面从后台回来且已暂停时保留选区；新页面才需要首次绘制。
    if (logState.follow || !box.dataset.initialized) paintLog();
    box.dataset.initialized = "true";
    logState.scrollTop = box.scrollTop;
    box.onscroll = () => {
      const previous = logState.scrollTop;
      logState.scrollTop = box.scrollTop;
      if (Math.abs(box.scrollTop - previous) < 1) return;
      if (logState.follow && box.scrollTop < previous && !logStuck(box)) setFollow(false);
      else if (!logState.follow && box.scrollTop > previous && logStuck(box)) setFollow(true);
    };
    logState.resize?.disconnect();
    logState.resize = new ResizeObserver(() => {
      if (logState.follow && box.isConnected && !document.hidden) logPin(box);
    });
    logState.resize.observe(box);
    if (document.hidden) return;
    connectLogs();
  }

  /* ---- 命令 ---- */

  const commandState = { output: "", history: [], busy: false };

  async function paintCommand() {
    const shortcuts = ["list", "diff ambient", "show oai", "defaults portrait"];
    return `
      ${pageHead("命令", "这里敲的和群里敲 /ctl 是同一套，以维护者身份执行。")}
      <section class="card">
        <p class="note">它只改这台机器上的配置，不往群里发消息——
        要给群里说话请回群里，或者用 agent 房间。</p>
        <form class="field" id="command-form">
          <input class="input input-mono" id="command-input" autocomplete="off"
                 spellcheck="false" placeholder="list / show ambient / set oai …"
                 aria-label="控制命令">
          <button class="btn btn-filled" type="submit">${ICONS.play}执行</button>
        </form>
        <div class="chips">
          ${shortcuts
            .map(
              (item) =>
                `<button class="chip" type="button" data-run="${esc(item)}">${esc(item)}</button>`
            )
            .join("")}
          ${commandState.history
            .map(
              (item) =>
                `<button class="chip" type="button" data-run="${esc(item)}"
                   title="再用一次">再用一次 · ${esc(item)}</button>`
            )
            .join("")}
        </div>
      </section>
      <section class="card">
        <div class="section-title">回执</div>
        <div class="code" id="command-output" role="status" aria-live="polite">${
          commandState.output ? esc(commandState.output) : "还没有执行过命令"
        }</div>
      </section>`;
  }

  async function runCommand(input) {
    if (commandState.busy || !input.trim()) return;
    commandState.busy = true;
    const button = $("#command-form [type=submit]");
    if (button) button.disabled = true;
    const output = $("#command-output");
    if (output) output.textContent = "执行中…";
    try {
      const data = await api("/command", { method: "POST", body: { input } });
      commandState.output = data.message;
      if (output) output.textContent = data.message;
      commandState.history = [input, ...commandState.history.filter((x) => x !== input)].slice(0, 3);
    } catch (error) {
      commandState.output = error.message;
      if (output) output.textContent = error.message;
      fail(error);
    } finally {
      commandState.busy = false;
      if (button) button.disabled = false;
    }
  }

  /* ---- 框架设置 ---- */

  async function paintSettings() {
    const data = await api("/settings");
    const bots = data.bots.length
      ? data.bots.map(renderBot).join("")
      : empty("还没有配连接；在下面加一条，或者先用本机控制台跑");

    return `
      ${pageHead("接入与全局", "这几项不在任何插件的配置里，/ctl 够不着，只在这一页改。")}

      <section class="card">
        <div class="section-title">装到桌面
          <span class="count">${standalone() ? "已经装上了" : "可选"}</span></div>
        ${installBody()}
      </section>

      <section class="card">
        <div class="section-title">阅读密度</div>
        <p class="note">紧凑字号在一屏呈现更多信息，舒适字号适合长时间阅读。仅保存在当前浏览器。</p>
        <div class="seg" data-connected role="group" aria-label="阅读密度">
          ${[["compact", "紧凑"], ["comfortable", "舒适"]].map(([value, label]) => `<button class="seg-item" type="button" data-density-choice="${value}" aria-pressed="${(document.documentElement.dataset.density || "compact") === value}">${label}</button>`).join("")}
        </div>
      </section>

      <section class="card">
        <div class="section-title">连接实现端
          <span class="count">${data.bots.length} 条 · 改完下次启动生效</span></div>
        <p class="note">知微自己不直接连 QQ：它连的是实现端（本机自建的那套在
        <span class="key">http://127.0.0.1:3001</span>）。这一份是机器人的「接在哪儿」，
        与群里的指令、插件配置都不相干。</p>
        <div class="list" id="bot-list">${bots}</div>
        <div class="actions">
          <button class="btn btn-tonal" type="button" data-add-bot>${ICONS.plus}加一条连接</button>
        </div>
      </section>

      <section class="card">
        <div class="section-title">全局</div>
        <form id="global-form" class="panel">
          <div class="kv">
            <span class="kv-key">command_prefix</span>
            <span class="kv-value"><input class="input input-mono" name="command_prefix"
              value="${esc(data.command_prefix.join(", "))}" spellcheck="false"
              aria-label="指令前缀"></span>
          </div>
          <div class="kv">
            <span class="kv-key">browser_path</span>
            <span class="kv-value"><input class="input input-mono" name="browser_path"
              value="${esc(data.browser_path)}" spellcheck="false"
              placeholder="留空即自动查找" aria-label="浏览器路径"></span>
          </div>
          <div class="kv kv-wide">
            <span class="kv-key">global_filter</span>
            <span class="kv-value">
              <button class="switch" type="button" role="switch"
                data-name="enable_blacklist" aria-checked="${data.global_filter.enable_blacklist}"
                aria-label="启用群黑名单"></button>
              <span class="group-label">黑名单</span>
            </span>
          </div>
          <div class="kv">
            <span class="kv-key">blacklist</span>
            <span class="kv-value"><input class="input input-mono" name="blacklist"
              value="${esc(data.global_filter.blacklist.join(", "))}" spellcheck="false"
              aria-label="群黑名单"></span>
          </div>
          <div class="kv kv-wide">
            <span class="kv-key">global_filter</span>
            <span class="kv-value">
              <button class="switch" type="button" role="switch"
                data-name="enable_whitelist" aria-checked="${data.global_filter.enable_whitelist}"
                aria-label="启用群白名单"></button>
              <span class="group-label">白名单</span>
            </span>
          </div>
          <div class="kv">
            <span class="kv-key">whitelist</span>
            <span class="kv-value"><input class="input input-mono" name="whitelist"
              value="${esc(data.global_filter.whitelist.join(", "))}" spellcheck="false"
              aria-label="群白名单"></span>
          </div>
        </form>
        <p class="note">两条名单都空 = 对所有群生效；同时出现在两边的群按禁止处理。
        逗号分开，例如 <span class="key">123456, 789012</span>。</p>
        <div class="actions">
          <button class="btn btn-filled" type="button" data-save-global>
            ${ICONS.save}保存全局设置</button>
        </div>
      </section>`;
  }

  /** 装到桌面：能直接叫出安装提示的浏览器给按钮，其余给步骤。 */
  function installBody() {
    if (standalone()) {
      return `<p class="note">它已经装在这台设备上了，这一页就是从桌面图标打开的那一份。</p>`;
    }
    const ua = navigator.userAgent;
    const steps = /iPhone|iPad|iPod/.test(ua)
      ? ["在 Safari 里打开这一个地址", "点底部的分享键", "选「添加到主屏幕」，名字留「知微」"]
      : /Android/.test(ua)
        ? ["用 Chrome 打开这一个地址", "点右上角的三点", "选「安装应用」或「添加到主屏幕」"]
        : ["地址栏右侧有一枚安装图标，点它", "或者用菜单里的「安装知微」", "装好后它会自己开一个窗口"];
    return `
      <p class="note">装上之后它跟普通应用一样：桌面有图标，打开就一个窗口，
      没有地址栏，顶栏直接贴到状态栏下面。装的是这一页，数据还是这台机器上的。</p>
      <ol class="list">
        ${steps
          .map(
            (step, index) => `
          <li class="row row-plain">
            <span class="row-icon">${index + 1}</span>
            <div class="row-body"><span class="row-sub">${esc(step)}</span></div>
          </li>`
          )
          .join("")}
      </ol>
      <div class="actions">
        <button class="btn btn-filled" type="button" data-install ${
          installPrompt ? "" : "hidden"
        }>${ICONS.install}现在就装</button>
      </div>`;
  }

  function renderBot(bot, index) {
    const order = index === "" ? "新的一条" : `第 ${Number(index) + 1} 条`;
    return `
      <form class="inset" id="bot-form-${index}" data-index="${index}">
        <div class="row row-plain">
          <div class="row-body">
            <span class="row-title">${order}
              ${bot.has_token ? `<span class="badge badge-on">已设令牌</span>` : ""}</span>
            <span class="row-sub">${esc(bot.protocol)} · ${esc(bot.url || "未填地址")}</span>
          </div>
          <div class="row-tail">
            <button class="switch" type="button" role="switch" data-name="enabled" name="enabled"
                    aria-checked="${bot.enabled}" aria-label="启用这条连接"></button>
          </div>
        </div>
        <div class="kv">
          <span class="kv-key">protocol</span>
          <span class="kv-value"><input class="input input-mono" name="protocol"
            value="${esc(bot.protocol)}" spellcheck="false" aria-label="协议"></span>
        </div>
        <div class="kv">
          <span class="kv-key">url</span>
          <span class="kv-value"><input class="input input-mono" name="url"
            value="${esc(bot.url)}" spellcheck="false" aria-label="实现端地址"></span>
        </div>
        <div class="kv">
          <span class="kv-key">access_token</span>
          <span class="kv-value"><input class="input input-mono" name="access_token"
            type="password" autocomplete="off" spellcheck="false"
            placeholder="${bot.has_token ? "已设置，留空即不动" : "留空表示不鉴权"}"
            aria-label="访问令牌"></span>
        </div>
        <div class="actions">
          <button class="btn btn-filled" type="submit">${ICONS.save}保存</button>
          <button class="btn btn-outline btn-danger" type="button" data-drop-bot="${index}">
            ${ICONS.trash}删掉这条</button>
        </div>
      </form>`;
  }

  function botPayload(form) {
    const value = (name) => {
      const field = form.querySelector(`[name="${name}"]`);
      return field ? field.value : "";
    };
    const on = form.querySelector('[name="enabled"]');
    const access = value("access_token");
    return {
      index: form.dataset.index === "" ? undefined : Number(form.dataset.index),
      enabled: on ? on.getAttribute("aria-checked") === "true" : true,
      protocol: value("protocol"),
      url: value("url"),
      // 没写字就不发这一格：后端按「不动原来那个」处理。
      ...(access.trim() ? { access_token: access } : {}),
    };
  }

  function globalPayload() {
    const value = (name) => {
      const field = $(`#global-form [name="${name}"]`);
      return field ? field.value : "";
    };
    const list = (name) =>
      value(name)
        .split(",")
        .map((part) => part.trim())
        .filter(Boolean)
        .map((part) => {
          const id = Number(part);
          if (!Number.isInteger(id)) throw new Error(`「${part}」不是群号`);
          return id;
        });
    const flag = (name) => {
      const button = $(`#global-form [data-name="${name}"]`);
      return button ? button.getAttribute("aria-checked") === "true" : false;
    };
    return {
      command_prefix: value("command_prefix")
        .split(",")
        .map((part) => part.trim())
        .filter(Boolean),
      browser_path: value("browser_path"),
      global_filter: {
        enable_blacklist: flag("enable_blacklist"),
        blacklist: list("blacklist"),
        enable_whitelist: flag("enable_whitelist"),
        whitelist: list("whitelist"),
      },
    };
  }

  /* ---- 解锁 ---- */

  function paintLock(reason) {
    token = "";
    store.clear();
    stopLogs();
    renderSeq++;
    $("#view").removeAttribute("aria-busy");
    $("#nav").innerHTML = "";
    // 地址栏里那份口令是旧的就别再留着：它会盖掉刚存下的新的那份（见 §3 的取值顺序），
    // 于是刷新一次又退回这里。抹掉之后只认 localStorage。
    if (new URLSearchParams(location.search).has("t")) {
      history.replaceState(null, "", location.pathname + location.hash);
    }
    $("#view").innerHTML = `
      <form class="lock" id="lock-form">
        <h1 class="section-title">${esc(NAME)}</h1>
        <p class="note">${esc(reason || "需要口令才能看这台机器的数据。")}
        启动日志里那条带 <span class="key">?t=</span> 的地址可以直接打开；
        口令本体在 <span class="key">data/console/token</span>。</p>
        <input class="input input-mono" id="lock-input" type="password"
               autocomplete="off" spellcheck="false" placeholder="粘贴口令" aria-label="口令">
        <button class="btn btn-filled" type="submit">解锁</button>
      </form>`;
    $("#lock-form").addEventListener("submit", (event) => {
      event.preventDefault();
      token = $("#lock-input").value.trim();
      if (!token) return;
      store.set(token);
      boot();
    });
    $("#lock-input").focus();
  }

  /* ==================== §10 事件 ==================== */

  /** 按下时从圆心漫开一片。只有指针设备与未开「减少动态效果」时才做。 */
  function ripple(event) {
    if (reduced.matches || event.button > 0) return;
    const host = event.target.closest(".btn, .nav-item, .seg-item, .chip, .row");
    if (!host || host.matches(":disabled") || host.matches(".row-plain:not(button)")) return;
    const box = host.getBoundingClientRect();
    const size = Math.max(box.width, box.height) * 2;
    const dot = document.createElement("span");
    dot.className = "ripple";
    dot.style.width = `${size}px`;
    dot.style.height = `${size}px`;
    dot.style.left = `${event.clientX - box.left - size / 2}px`;
    dot.style.top = `${event.clientY - box.top - size / 2}px`;
    dot.addEventListener("animationend", () => dot.remove(), { once: true });
    host.appendChild(dot);
  }

  async function commitField(control, forced) {
    if (control.disabled) return;
    const path = control.dataset.path;
    const name = control.closest("[data-config-plugin]")?.dataset.configPlugin;
    if (!name) return;
    let value;
    try {
      value =
        control.dataset.kind === "bool"
          ? Boolean(forced)
          : parseValue(control.dataset.kind, control.value);
    } catch (error) {
      snack(error.message, "bad");
      return;
    }
    control.disabled = true;
    try {
      const data = await api(`/plugins/${encodeURIComponent(name)}/config`, {
        method: "POST",
        body: { path, value },
      });
      snack(data.message);
      if (control.dataset.kind === "bool") control.setAttribute("aria-checked", String(value));
    } catch (error) {
      fail(error);
      render();
    } finally {
      control.disabled = false;
    }
  }

  async function togglePlugin(toggle) {
    if (toggle.disabled) return;
    toggle.disabled = true;
    const next = toggle.getAttribute("aria-checked") !== "true";
    try {
      const data = await api(`/plugins/${encodeURIComponent(toggle.dataset.toggle)}/enabled`, {
        method: "POST",
        body: { on: next },
      });
      snack(data.message);
      toggle.setAttribute("aria-checked", String(next));
      if (toggle.isConnected) await render();
    } catch (error) {
      toggle.setAttribute("aria-checked", String(!next));
      fail(error);
    } finally {
      toggle.disabled = false;
    }
  }

  /** 宽屏上换一个插件只重画右边那一格，列表与搜索词都留在原地。 */
  let detailSeq = 0;
  async function openDetail(name) {
    const seq = ++detailSeq;
    pluginView.selected = name;
    for (const row of document.querySelectorAll("#plugin-list [data-plugin]")) {
      if (row.dataset.plugin === name) row.setAttribute("aria-selected", "true");
      else row.removeAttribute("aria-selected");
    }
    const pane = $("#plugin-detail");
    if (!pane) return;
    pane.innerHTML = skeleton(2);
    try {
      const plugin = await fetchPlugin(name);
      if (seq !== detailSeq || !pane.isConnected) return;
      pane.innerHTML = pluginDetailHtml(plugin);
      $(".bar-title").textContent = plugin.display;
      document.title = `${plugin.display} · ${NAME}`;
    } catch (error) {
      if (seq !== detailSeq || !pane.isConnected) return;
      pane.innerHTML = empty(error && error.message ? error.message : "这一个没能读出来");
    }
    history.replaceState(null, "", `#/plugins/${encodeURIComponent(name)}`);
  }

  async function runSource(which) {
    const box = $("#" + which);
    snack("正在写这两个文件…", "busy");
    try {
      const data = await api("/ambient/source", {
        method: "POST",
        body: { name: which, text: box ? box.value : "" },
      });
      snack(data.message);
    } catch (error) {
      fail(error);
    }
  }

  function bindEvents() {
    document.addEventListener("pointerdown", ripple, { passive: true });

    document.addEventListener("click", async (event) => {
      const target = event.target;

      if (target.closest(".skip-link")) {
        event.preventDefault();
        $("#view").focus();
        return;
      }

      if (target.closest("[data-snack-close]")) {
        clearTimeout(snackTimer);
        snackHost().innerHTML = "";
        return;
      }

      // 宽屏上列表与详情并排：点一行只换右边，别整页重来。
      const pluginRow = target.closest("[data-plugin]");
      if (pluginRow && !target.closest("button") && layout() === "expanded" && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        await openDetail(pluginRow.dataset.plugin);
        return;
      }

      const flag = target.closest('button[data-path][data-kind="bool"]');
      if (flag) {
        await commitField(flag, flag.getAttribute("aria-checked") !== "true");
        return;
      }

      // 设置页里那几个开关只改表单状态，等按保存才提交。
      const formFlag = target.closest("button[data-name]");
      if (formFlag) {
        const on = formFlag.getAttribute("aria-checked") === "true";
        formFlag.setAttribute("aria-checked", String(!on));
        return;
      }

      const toggle = target.closest("[data-toggle]");
      if (toggle) {
        await togglePlugin(toggle);
        return;
      }

      const drop = target.closest("[data-drop-bot]");
      if (drop) {
        // 草稿（还没落过配置的那一条）的 index 是空串，Number("") 会静默变成 0，
        // 于是「删草稿」会去删配置里的第一条真连接。草稿只活在 DOM 里，直接撤掉表单。
        const raw = drop.dataset.dropBot;
        if (raw === "") {
          drop.closest("form")?.remove();
          return;
        }
        const index = Number(raw);
        const ok = await ask({
          title: `删掉第 ${index + 1} 条连接？`,
          body: "删掉之后这一条不再连；改动要下次启动才生效。",
          confirm: "删掉",
          danger: true,
        });
        if (!ok) return;
        try {
          const data = await api("/settings/bot", {
            method: "POST",
            body: { index, remove: true, enabled: false, protocol: "satori", url: "" },
          });
          snack(data.message);
          render();
        } catch (error) {
          fail(error);
        }
        return;
      }

      if (target.closest("[data-add-bot]")) {
        const list = $("#bot-list");
        if (list && !$('[data-index=""]')) {
          // 新增这一条先摆进页面，填好按它自己的保存才落到配置里。
          list.insertAdjacentHTML(
            "beforeend",
            renderBot({ protocol: "satori", url: "", enabled: true }, "")
          );
        }
        return;
      }

      if (target.closest("[data-save-global]")) {
        try {
          const data = await api("/settings/global", { method: "POST", body: globalPayload() });
          snack(data.message);
        } catch (error) {
          fail(error);
        }
        return;
      }

      const density = target.closest("[data-density-choice]");
      if (density) {
        applyDensity(density.dataset.densityChoice);
        try { localStorage.setItem("zhiwei.density", density.dataset.densityChoice); } catch { /* private mode */ }
        return;
      }

      if (target.closest("[data-install]")) {
        await install();
        return;
      }

      const section = target.closest("[data-section]");
      if (section) {
        pluginView.section = section.dataset.section;
        repaintPlugins();
        return;
      }

      const level = target.closest("[data-level]");
      if (level) {
        logState.level = level.dataset.level;
        for (const item of document.querySelectorAll("[data-level]")) {
          item.setAttribute("aria-pressed", String(item === level));
        }
        paintLog();
        return;
      }

      if (target.closest("#log-follow")) {
        setFollow(!logState.follow);
        return;
      }

      if (target.closest("#log-jump")) {
        setFollow(true);
        return;
      }

      if (target.closest("#log-export")) {
        const text = logState.lines.filter(logMatches).map(line => `[${line.at}] [${line.level}] [${line.target}] ${line.text}`).join("\n");
        const url = URL.createObjectURL(new Blob([text + "\n"], { type: "text/plain;charset=utf-8" }));
        const link = document.createElement("a");
        link.href = url;
        link.download = `zhiwei-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        return;
      }

      if (target.closest("#log-clear")) {
        logState.lines = [];
        logState.queue = [];
        paintLog();
        return;
      }

      const reset = target.closest("[data-reset]");
      if (reset) {
        const name = reset.dataset.reset;
        const ok = await ask({
          title: `恢复「${name}」的默认参数？`,
          body: "这会覆盖它现在所有的取值，插件自己的开关保留。",
          confirm: "恢复默认",
          danger: true,
        });
        if (!ok) return;
        try {
          const data = await api(`/plugins/${encodeURIComponent(name)}/reset`, {
            method: "POST",
            body: {},
          });
          snack(data.message);
          render();
        } catch (error) {
          fail(error);
        }
        return;
      }

      const source = target.closest("[data-source]");
      if (source) {
        await runSource(source.dataset.source);
        return;
      }

      const copy = target.closest("[data-copy]");
      if (copy) {
        try {
          await navigator.clipboard.writeText(copy.dataset.copy);
          snack(`已复制 ${copy.dataset.copy}`);
        } catch {
          snack("这台设备没给剪贴板权限；长按选中也可以", "bad");
        }
        return;
      }

      const shortcut = target.closest("[data-run]");
      if (shortcut) {
        const input = $("#command-input");
        if (input) input.value = shortcut.dataset.run;
        runCommand(shortcut.dataset.run);
      }
    });

    document.addEventListener("change", async (event) => {
      const control = event.target.closest("[data-path]");
      if (!control || control.dataset.kind === "bool") return;
      await commitField(control);
    });

    document.addEventListener("keydown", async (event) => {
      const typing =
        event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement;
      if (event.isComposing || $("#dialog[open]")) return;
      if (event.key === "Enter") {
        const control = event.target.closest ? event.target.closest("input[data-path]") : null;
        if (control) {
          event.preventDefault();
          await commitField(control);
          return;
        }
        return;
      }
      if (typing || event.metaKey || event.ctrlKey || event.altKey) return;
      const index = Number(event.key);
      if (index >= 1 && index <= PAGES.length) {
        go(PAGES[index - 1].id);
        return;
      }
      if (event.key === "/") {
        const search = $("#plugin-search") || $("#log-search");
        if (search) {
          event.preventDefault();
          search.focus();
        }
        return;
      }
      if (event.key === "Escape") {
        const box = $("#log-box");
        if (box && document.activeElement === box) box.blur();
      }
    });

    document.addEventListener("input", (event) => {
      if (event.target.closest("#plugin-search")) {
        pluginView.text = event.target.value;
        repaintPlugins();
        return;
      }
      if (event.target.closest("#log-search")) {
        logState.text = event.target.value;
        paintLog();
      }
    });

    // 设置页的两张表单：连接的每一条各一张，全局一张。都等按保存才提交，
    // 回车不提交（全局那张没有提交按钮，不拦下来会整页刷新）。
    document.addEventListener("submit", async (event) => {
      if (event.target.id === "command-form") {
        event.preventDefault();
        const input = $("#command-input");
        if (input.value.trim()) await runCommand(input.value.trim());
        return;
      }
      if (event.target.id === "global-form") {
        event.preventDefault();
        return;
      }
      const botForm = event.target.closest("form[data-index]");
      if (!botForm) return;
      event.preventDefault();
      try {
        const data = await api("/settings/bot", { method: "POST", body: botPayload(botForm) });
        snack(data.message);
        render();
      } catch (error) {
        fail(error);
      }
    });

    // Android 切回 Termux 时释放 SSE；回来由服务端快照补齐，后台零日志解析。
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) stopLogs();
      else if (token) {
        if ($("#log-box")) mountLogs();
        if ($("#overview-log")) mountOverviewLog();
      }
    });
    window.addEventListener("pagehide", stopLogs);
    window.addEventListener("pageshow", () => {
      if (document.hidden) return;
      if (token && $("#log-box")) mountLogs();
      if (token && $("#overview-log")) mountOverviewLog();
    });

    prefersDark.addEventListener("change", applyTheme);
    for (const media of [narrow, wide]) {
      media.addEventListener("change", () => {
        applyLayout();
        render();
      });
    }
    window.addEventListener("hashchange", render);
  }

  /* ==================== §11 启动 ==================== */

  let installPrompt = null;

  async function install() {
    if (!installPrompt) return;
    installPrompt.prompt();
    const { outcome } = await installPrompt.userChoice;
    installPrompt = null;
    snack(outcome === "accepted" ? "已交给系统安装；装好之后桌面会多一个知微" : "这次没有装，随时可以再来");
    render();
  }

  function applyDensity(value) {
    document.documentElement.dataset.density = value === "comfortable" ? "comfortable" : "compact";
    for (const button of document.querySelectorAll("[data-density-choice]")) {
      button.setAttribute("aria-pressed", String(button.dataset.densityChoice === document.documentElement.dataset.density));
    }
  }

  function boot() {
    try { applyDensity(localStorage.getItem("zhiwei.density")); } catch { applyDensity("compact"); }
    applyTheme();
    applyLayout();

    $("#refresh").innerHTML = ICONS.refresh;
    $("#refresh").onclick = (event) => {
      const button = event.currentTarget;
      if (button.disabled) return;
      button.disabled = true;
      button.dataset.busy = "";
      render().finally(() => { delete button.dataset.busy; button.disabled = false; });
    };
    $("#settings").innerHTML = ICONS.gear;
    $("#settings").onclick = () => go("settings");

    buildNav();
    render();
  }

  window.addEventListener("beforeinstallprompt", (event) => {
    event.preventDefault();
    installPrompt = event;
    const button = $("[data-install]");
    if (button) button.hidden = false;
  });

  document.addEventListener("DOMContentLoaded", () => {
    bindEvents();
    if (!token) {
      paintLock();
      return;
    }
    boot();
  });
})();
