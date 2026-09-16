/* ============================================================================
   知言控制台 · 前端
   ----------------------------------------------------------------------------
   一个文件、零依赖、零构建。理由与后端把资源编译进二进制是同一条：这是跑在
   别人机器上的机器人的界面，不该指望任何一台 CDN 活着，也不该为它引入一套
   打包链。全篇只做三件事——取数据、拼字符串、按 hash 换页。

   三处纪律：
   - 所有插值一律走 esc()。页面上的字有一半来自配置、日志与人名，其中任何
     一处漏转义都是一个注入点；
   - 不自己造状态。开关、配置、日志都以后端为准，改完重新拉一次，不猜结果。
     唯一的例外是日志缓冲——它只增不改，重画时不能把已经到过的行丢掉；
   - 不往 DOM 里塞 style 字面量。视觉值只在 res/console/app.css 里，这里只
     挑类名。
   ========================================================================== */

(() => {
  "use strict";

  const NAME = "知言";
  const TOKEN_KEY = "zhiyan.token";

  /* ------------------------------ 图标 ------------------------------ */
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
  };

  /* ------------------------------ 小工具 ------------------------------ */

  const $ = (selector, root = document) => root.querySelector(selector);

  const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]);

  const num = (value) => Number(value ?? 0).toLocaleString("zh-CN");

  /** 时长说成人话：控制台上一行要读得下去，不摆秒数。 */
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

  /* ------------------------------ 口令 ------------------------------ */

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

  /* ------------------------------ 网络 ------------------------------ */

  async function api(path, options = {}) {
    const init = { method: options.method || "GET", headers: { "x-zhiyan-token": token } };
    if (options.body !== undefined) {
      init.headers["content-type"] = "application/json";
      init.body = JSON.stringify(options.body);
    }
    const response = await fetch(`/api${path}`, init);
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(payload.error || `服务返回 ${response.status}`);
      error.status = response.status;
      throw error;
    }
    return payload;
  }

  /* ------------------------------ 反馈条 ------------------------------ */

  let snackTimer = 0;
  function snack(text, kind = "good") {
    $("#snack").innerHTML = `<div class="zy-snack zy-snack-${kind}">${esc(text)}</div>`;
    clearTimeout(snackTimer);
    // 报错多留一会儿：一句话要读完，还要来得及照它做。
    snackTimer = setTimeout(() => ($("#snack").innerHTML = ""), kind === "bad" ? 8000 : 3600);
  }

  function fail(error) {
    if (error && error.status === 401) {
      store.clear();
      renderLock("口令不对，或者它已经换过了");
      return;
    }
    snack(error && error.message ? error.message : String(error), "bad");
  }

  /* ------------------------------ 主题 ------------------------------ */

  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)");
  function applyTheme() {
    document.body.classList.toggle("dark", prefersDark.matches);
  }
  if (prefersDark.addEventListener) prefersDark.addEventListener("change", applyTheme);

  /* ------------------------------ 解锁页 ------------------------------ */

  function renderLock(reason) {
    $("#nav").innerHTML = "";
    $("#view").innerHTML = `
      <form class="zy-lock" id="lock-form">
        <div class="zy-title">${esc(NAME)}</div>
        <p class="zy-note">${esc(reason || "需要口令才能看这台机器的数据。")}
        启动日志里那条带 <span class="zy-key">?t=</span> 的地址可以直接打开；
        口令本体在 <span class="zy-key">data/console/token</span>。</p>
        <input class="zy-input zy-input-mono" id="lock-input" type="password"
               autocomplete="off" spellcheck="false" placeholder="粘贴口令" aria-label="口令">
        <button class="zy-btn zy-btn-filled" type="submit">解锁</button>
      </form>`;
    $("#lock-form").addEventListener("submit", (event) => {
      event.preventDefault();
      token = $("#lock-input").value.trim();
      if (!token) return;
      store.set(token);
      render();
    });
  }

  /* ------------------------------ 外壳 ------------------------------ */

  const PAGES = [
    { id: "overview", label: "总览" },
    { id: "plugins", label: "插件" },
    { id: "ambient", label: "搭话" },
    { id: "logs", label: "日志" },
    { id: "command", label: "命令" },
  ];

  function renderNav(active) {
    $("#nav").innerHTML = PAGES.map(
      (page) => `
      <button class="zy-nav-item" type="button" data-nav="${page.id}"
              ${page.id === active ? 'aria-current="page"' : ""}>
        ${ICONS[page.id]}<span>${page.label}</span>
      </button>`
    ).join("");
  }

  /* ------------------------------ 通用块 ------------------------------ */

  const empty = (text) =>
    `<div class="zy-empty"><div class="zy-empty-icon">📭</div>
     <div class="zy-empty-text">${esc(text)}</div></div>`;

  const skeleton = (rows = 3) =>
    `<div class="zy-card">${'<div class="zy-skeleton zy-skeleton-lg"></div>'.repeat(rows)}</div>`;

  const reading = (key, value, unit = "") =>
    `<div class="zy-reading"><span class="zy-reading-key">${esc(key)}</span>
     <span class="zy-reading-value">${esc(value)}${
       unit ? `<span class="zy-reading-unit">${esc(unit)}</span>` : ""
     }</span></div>`;

  /* ------------------------------ 页面：总览 ------------------------------ */

  async function pageOverview() {
    const data = await api("/overview");
    const bots = data.bots.length
      ? data.bots
          .map(
            (bot) => `
        <div class="zy-row zy-row-plain">
          <div class="zy-row-body">
            <span class="zy-row-title">${esc(bot.name || bot.nick || bot.id || "未取得账号")}
              <span class="zy-key">${esc(bot.adapter)}/${esc(bot.platform)}</span>
            </span>
            <span class="zy-row-sub">${
              bot.id ? `已连上，账号 ${esc(bot.id)}` : "已连接实现端，账号还没认出来"
            }</span>
          </div>
        </div>`
          )
          .join("")
      : empty("还没有连接；启动日志里找「启动适配器」那几行");

    return `
      <section class="zy-hero">
        <span class="zy-hero-note">${esc(NAME)} · ${esc(data.app.version)}</span>
        <span class="zy-hero-name">已经跑了 ${esc(span(data.app.uptime))}</span>
        <span class="zy-hero-note">这一次是 ${esc(data.app.started)} 起来的 ·
          控制台在 ${esc(data.console.address)}</span>
      </section>

      <section class="zy-card">
        <div class="zy-section-title">此刻</div>
        <div class="zy-readings">
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

      <section class="zy-card">
        <div class="zy-section-title">连接<span class="zy-count">${data.bots.length}</span></div>
        <div class="zy-list">${bots}</div>
      </section>

      <section class="zy-card">
        <div class="zy-section-title">最近发生了什么
          <span class="zy-count">最近 6 行</span></div>
        <div class="zy-log zy-log-short" id="overview-log"></div>
        <button class="zy-btn zy-btn-tonal" type="button" data-nav="logs">去日志页看全部</button>
      </section>`;
  }

  async function mountOverview() {
    const box = $("#overview-log");
    if (!box) return;
    try {
      const data = await api("/logs");
      const lines = (data.lines || []).slice(-6);
      box.innerHTML = lines.length
        ? lines.map(logLine).join("")
        : `<div class="zy-log-line zy-log-debg">这里还是空的</div>`;
    } catch {
      /* 总览里这一格不重要，取不到就让它空着 */
    }
  }

  /* ------------------------------ 页面：插件 ------------------------------ */

  const pluginView = { payload: null, text: "", section: "" };

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
        <div class="zy-row" data-plugin="${esc(plugin.name)}" role="button" tabindex="0">
          <div class="zy-row-body">
            <span class="zy-row-title">${esc(plugin.display)}
              <span class="zy-key">${esc(plugin.name)}</span>
              ${badge(plugin)}
            </span>
            <span class="zy-row-sub">${esc(plugin.summary)}</span>
          </div>
          <div class="zy-row-tail">
            <button class="zy-switch" type="button" role="switch" data-toggle="${esc(plugin.name)}"
                    aria-checked="${plugin.on}" aria-label="启用或停用${esc(plugin.display)}"
                    ${plugin.name === "ctl" ? "disabled" : ""}></button>
            <span class="zy-chevron">${ICONS.chevron}</span>
          </div>
        </div>`
      )
      .join("");
    return `<div class="zy-list">${rows || empty("没有符合条件的插件")}</div>`;
  }

  function pluginChips() {
    const data = pluginView.payload;
    return [
      `<button class="zy-chip" type="button" data-section=""
         aria-pressed="${pluginView.section === ""}">全部 ${data.plugins.length}</button>`,
      ...data.sections
        .map((section) => {
          const count = data.plugins.filter((plugin) => plugin.section === section.code).length;
          if (!count) return "";
          return `<button class="zy-chip" type="button" data-section="${esc(section.code)}"
            aria-pressed="${pluginView.section === section.code}">${esc(section.name)} ${count}</button>`;
        })
        .filter(Boolean),
    ].join("");
  }

  /** 只重画列表与筛选项：敲一个字就整页重拉一次太浪费，也会把光标顶掉。 */
  function repaintPlugins() {
    const list = $("#plugin-list");
    if (list) list.outerHTML = `<section class="zy-card" id="plugin-list">${pluginRows()}</section>`;
    const chips = $("#plugin-chips");
    if (chips) chips.innerHTML = pluginChips();
  }

  async function pagePlugins() {
    pluginView.payload = await api("/plugins");
    return `
      <div class="zy-search">
        ${ICONS.search}
        <input id="plugin-search" type="search" placeholder="按名字或说明找"
               value="${esc(pluginView.text)}" aria-label="搜索插件">
      </div>
      <div class="zy-chips" id="plugin-chips">${pluginChips()}</div>
      <section class="zy-card" id="plugin-list">${pluginRows()}</section>`;
  }

  function badge(plugin) {
    if (plugin.pending) return `<span class="zy-badge zy-badge-pending">待重启</span>`;
    return plugin.on
      ? `<span class="zy-badge zy-badge-on">已启用</span>`
      : `<span class="zy-badge zy-badge-off">已停用</span>`;
  }

  /* ------------------------------ 页面：插件详情 ------------------------------ */

  async function pagePlugin(name) {
    const plugin = await api(`/plugins/${encodeURIComponent(name)}`);
    const rows = Object.entries(plugin.config)
      .map(([key, value]) => renderNode(key, key, value))
      .join("");
    const differences = plugin.diff.length
      ? `<div class="zy-code">${esc(plugin.diff.join("\n"))}</div>`
      : `<p class="zy-note">与默认值一致。</p>`;
    const commands = plugin.commands.length
      ? plugin.commands
          .map(
            (command) => `
            <div class="zy-row zy-row-plain" data-copy="${esc(command.cmd)}" role="button"
                 tabindex="0" title="点击复制">
              <div class="zy-row-body">
                <span class="zy-row-title"><span class="zy-key">${esc(command.cmd)}</span></span>
                <span class="zy-row-sub">${esc(command.note)}</span>
              </div>
            </div>`
          )
          .join("")
      : `<p class="zy-note">这个插件没有指令，它在后台按排期自己工作。</p>`;

    return `
      <button class="zy-btn zy-btn-tonal zy-back" type="button" data-nav="plugins">
        ${ICONS.back}回插件列表</button>
      <section class="zy-card">
        <div class="zy-title">${esc(plugin.display)}</div>
        <p class="zy-note">${esc(plugin.summary)}</p>
        <div class="zy-row zy-row-plain">
          <div class="zy-row-body">
            <span class="zy-row-title">${badge(plugin)}</span>
            <span class="zy-row-sub">${esc(plugin.effect)}</span>
          </div>
          <button class="zy-switch" type="button" role="switch" data-toggle="${esc(plugin.name)}"
                  aria-checked="${plugin.on}" aria-label="启用或停用"
                  ${plugin.name === "ctl" ? "disabled" : ""}></button>
        </div>
      </section>

      <div class="zy-split">
        <section class="zy-card">
          <div class="zy-section-title">配置<span class="zy-count">改了立刻生效</span></div>
          <div class="zy-panel">${rows}</div>
          <button class="zy-btn zy-btn-outline" type="button" data-reset="${esc(plugin.name)}"
                  data-confirm="恢复参数默认值会覆盖现有取值，继续？">
            恢复默认参数（保留开关）</button>
        </section>
        <section class="zy-card">
          <div class="zy-section-title">和默认差在哪</div>
          ${differences}
          <div class="zy-section-title">指令
            <span class="zy-count">${plugin.commands.length} 条 · 点一条复制</span></div>
          <div class="zy-list">${commands}</div>
        </section>
      </div>`;
  }

  /** 一个配置节点：表与对象数组往下拆，叶子就地渲染成能改的控件。 */
  function renderNode(path, key, value) {
    if (Array.isArray(value) && value.every((item) => item === null || typeof item !== "object")) {
      return kv(
        path,
        key,
        `<input class="zy-input zy-input-mono" data-path="${esc(path)}" data-kind="list"
          value="${esc(value.join(", "))}" spellcheck="false" aria-label="${esc(key)}">`
      );
    }
    if (value !== null && typeof value === "object") {
      const inner = Array.isArray(value)
        ? value.map((item, index) => renderNode(`${path}.${index}`, `[${index}]`, item)).join("")
        : Object.entries(value)
            .map(([child, item]) => renderNode(`${path}.${child}`, child, item))
            .join("");
      return kv(path, key, `<div class="zy-subtable">${inner}</div>`);
    }
    if (typeof value === "boolean") {
      return kv(
        path,
        key,
        `<button class="zy-switch" type="button" role="switch" data-path="${esc(path)}"
           data-kind="bool" aria-checked="${value}" aria-label="${esc(key)}"></button>`
      );
    }
    // 长文本（提示词、人格那类）给一块多行的地方，单行输入框里换行会被吃掉。
    const long = typeof value === "string" && (value.includes("\n") || value.length > 120);
    if (long) {
      return `<div class="zy-kv zy-kv-wide">
        <span class="zy-kv-key" title="${esc(path)}">${esc(key)}</span>
        <textarea class="zy-area zy-area-short" data-path="${esc(path)}" data-kind="string"
                  spellcheck="false" aria-label="${esc(key)}">${esc(value)}</textarea></div>`;
    }
    const kind = typeof value === "number" ? "number" : "string";
    return kv(
      path,
      key,
      `<input class="zy-input zy-input-mono" data-path="${esc(path)}" data-kind="${kind}"
        value="${esc(value ?? "")}" spellcheck="false" aria-label="${esc(key)}">`
    );
  }

  const kv = (path, key, control) =>
    `<div class="zy-kv"><span class="zy-kv-key" title="${esc(path)}">${esc(key)}</span>
     <span class="zy-kv-value">${control}</span></div>`;

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

  /* ------------------------------ 页面：搭话 ------------------------------ */

  async function pageAmbient() {
    const data = await api("/ambient");
    if (!data.ready) return empty("搭话插件的目录还没建起来，先让机器人跑一轮");

    const groups = data.memory.length
      ? data.memory.map(renderGroupMemory).join("")
      : empty("还没记住任何群");
    const gallery = data.stickers.length
      ? data.stickers.map(renderSticker).join("")
      : empty("库里还没有东西；它在群里收下一张就会存一张");

    return `
      <section class="zy-card">
        <div class="zy-section-title">它在群里像谁
          <span class="zy-count">改完下一轮生效</span></div>
        <p class="zy-note">人格是它在群里说话的样子，档案是它知道自己是谁。
        两份都直接写进运行目录，旧的那份存成同名的 backup；
        线上那份与仓库里那份是两回事，改这里不动仓库。</p>
        <label class="zy-note" for="persona">人格 persona.md</label>
        <textarea class="zy-area" id="persona" spellcheck="false">${esc(data.persona)}</textarea>
        <label class="zy-note" for="self">档案 self.md</label>
        <textarea class="zy-area zy-area-short" id="self" spellcheck="false">${esc(
          data.self
        )}</textarea>
        <div class="zy-chips">
          <button class="zy-btn zy-btn-filled" type="button" data-source="persona">
            ${ICONS.save}保存人格</button>
          <button class="zy-btn zy-btn-outline" type="button" data-source="self">
            ${ICONS.save}保存档案</button>
        </div>
      </section>

      <section class="zy-card">
        <div class="zy-section-title">记得什么
          <span class="zy-count">${data.memory.length} 个群</span></div>
        <div class="zy-list">${groups}</div>
      </section>

      <section class="zy-card">
        <div class="zy-section-title">表情包库
          <span class="zy-count">${data.stickers.length} 张</span></div>
        <div class="zy-gallery">${gallery}</div>
      </section>`;
  }

  function renderSticker(sticker) {
    return `
      <div class="zy-tile">
        ${
          sticker.image
            ? `<img class="zy-tile-img" loading="lazy" alt="${esc(sticker.label)}"
                 src="/api/ambient/sticker/${sticker.id}?t=${encodeURIComponent(token)}">`
            : `<div class="zy-tile-img zy-tile-blank">商城表情<br>只存了参数</div>`
        }
        <span class="zy-tile-label">${esc(sticker.label || "没起名的表情包")}</span>
        <span class="zy-note">#${sticker.id} · 用过 ${sticker.uses} 次 · ${esc(
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
        <div class="zy-row zy-row-plain">
          <div class="zy-row-body">
            <span class="zy-row-title">${esc(person.name || person.id)}
              ${person.address ? `<span class="zy-key">叫「${esc(person.address)}」</span>` : ""}
            </span>
            <span class="zy-row-sub">${esc(person.note || "有称呼，没写印象")}</span>
          </div>
          <div class="zy-row-tail">
            <span class="zy-note">${num(person.messages)} 条 · ${esc(ago(person.last_seen))}</span>
          </div>
        </div>`
      )
      .join("");
    const notes = group.notes
      .map(
        (note) => `
        <div class="zy-row zy-row-plain">
          <div class="zy-row-body"><span class="zy-row-sub">${esc(note.text)}</span></div>
          <div class="zy-row-tail"><span class="zy-note">${esc(ago(note.at))}</span></div>
        </div>`
      )
      .join("");

    return `
      <div class="zy-inset">
        <div class="zy-section-title">群 ${esc(group.group)}
          <span class="zy-count">${group.people.length} 人 · ${group.notes.length} 条旧事</span>
        </div>
        <p class="zy-note">${
          noted.length
            ? `有印象的 ${noted.length} 位${others > 0 ? `，另有 ${others} 位只记得露过面` : ""}`
            : "这个群里它还谁都没写上印象"
        }</p>
        <div class="zy-list">${people}${notes}</div>
      </div>`;
  }

  /* ------------------------------ 页面：设置 ------------------------------ */

  async function pageSettings() {
    const data = await api("/settings");
    const bots = data.bots.length
      ? data.bots.map(renderBot).join("")
      : empty("还没有配连接；在下面加一条，或者先用本机控制台跑");

    return `
      <button class="zy-btn zy-btn-tonal zy-back" type="button" data-nav="overview">
        ${ICONS.back}回总览</button>

      <section class="zy-card">
        <div class="zy-section-title">连接实现端
          <span class="zy-count">${data.bots.length} 条 · 改完下次启动生效</span></div>
        <p class="zy-note">知言自己不直接连 QQ：它连的是实现端（本机自建的那套在
        <span class="zy-key">http://127.0.0.1:3001</span>）。这一份是机器人的「接在哪儿」，
        与群里的指令、插件配置都不相干。</p>
        <div class="zy-list">${bots}</div>
      </section>

      <section class="zy-card">
        <div class="zy-section-title">全局</div>
        <form id="global-form" class="zy-panel">
          <div class="zy-kv">
            <span class="zy-kv-key">command_prefix</span>
            <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="command_prefix"
              value="${esc(data.command_prefix.join(", "))}" spellcheck="false"
              aria-label="指令前缀"></span>
          </div>
          <div class="zy-kv">
            <span class="zy-kv-key">browser_path</span>
            <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="browser_path"
              value="${esc(data.browser_path)}" spellcheck="false"
              placeholder="留空即自动查找" aria-label="浏览器路径"></span>
          </div>
          <div class="zy-kv">
            <span class="zy-kv-key">global_filter.enable_blacklist</span>
            <span class="zy-kv-value"><button class="zy-switch" type="button" role="switch"
              data-name="enable_blacklist" aria-checked="${data.global_filter.enable_blacklist}"
              aria-label="启用群黑名单"></button></span>
          </div>
          <div class="zy-kv">
            <span class="zy-kv-key">global_filter.blacklist</span>
            <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="blacklist"
              value="${esc(data.global_filter.blacklist.join(", "))}" spellcheck="false"
              aria-label="群黑名单"></span>
          </div>
          <div class="zy-kv">
            <span class="zy-kv-key">global_filter.enable_whitelist</span>
            <span class="zy-kv-value"><button class="zy-switch" type="button" role="switch"
              data-name="enable_whitelist" aria-checked="${data.global_filter.enable_whitelist}"
              aria-label="启用群白名单"></button></span>
          </div>
          <div class="zy-kv">
            <span class="zy-kv-key">global_filter.whitelist</span>
            <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="whitelist"
              value="${esc(data.global_filter.whitelist.join(", "))}" spellcheck="false"
              aria-label="群白名单"></span>
          </div>
        </form>
        <p class="zy-note">两条名单都空 = 对所有群生效；同时出现在两边的群按禁止处理。
        逗号分开，例如 <span class="zy-key">123456, 789012</span>。</p>
        <button class="zy-btn zy-btn-filled" type="button" data-save-global>
          ${ICONS.save}保存全局设置</button>
      </section>`;
  }

  function renderBot(bot, index) {
    return `
      <form class="zy-inset" id="bot-form-${index}" data-index="${index}">
        <div class="zy-row zy-row-plain">
          <div class="zy-row-body">
            <span class="zy-row-title">第 ${index + 1} 条
              ${bot.has_token ? `<span class="zy-badge zy-badge-on">已设令牌</span>` : ""}</span>
            <span class="zy-row-sub">${esc(bot.protocol)} · ${esc(bot.url || "未填地址")}</span>
          </div>
          <div class="zy-row-tail">
            <button class="zy-switch" type="button" role="switch" name="enabled"
                    aria-checked="${bot.enabled}" aria-label="启用这条连接"></button>
          </div>
        </div>
        <div class="zy-kv">
          <span class="zy-kv-key">protocol</span>
          <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="protocol"
            value="${esc(bot.protocol)}" spellcheck="false" aria-label="协议"></span>
        </div>
        <div class="zy-kv">
          <span class="zy-kv-key">url</span>
          <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="url"
            value="${esc(bot.url)}" spellcheck="false" aria-label="实现端地址"></span>
        </div>
        <div class="zy-kv">
          <span class="zy-kv-key">access_token</span>
          <span class="zy-kv-value"><input class="zy-input zy-input-mono" name="access_token"
            type="password" autocomplete="off" spellcheck="false"
            placeholder="${bot.has_token ? "已设置，留空即不动" : "留空表示不鉴权"}"
            aria-label="访问令牌"></span>
        </div>
        <div class="zy-chips">
          <button class="zy-btn zy-btn-filled" type="submit">${ICONS.save}保存</button>
          <button class="zy-btn zy-btn-outline" type="button" data-drop-bot="${index}">
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
    const token = value("access_token");
    return {
      index: Number(form.dataset.index),
      enabled: on ? on.getAttribute("aria-checked") === "true" : true,
      protocol: value("protocol"),
      url: value("url"),
      // 没写字就不发这一格：后端按「不动原来那个」处理。
      ...(token.trim() ? { access_token: token } : {}),
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

  /* ------------------------------ 页面：日志 ------------------------------ */

  const logState = {
    lines: [],
    loaded: false,
    level: "",
    text: "",
    follow: true,
    source: null,
    limit: 2000,
  };

  function logLine(entry) {
    const level = String(entry.level || "INFO").toLowerCase();
    return `<div class="zy-log-line zy-log-${esc(level)}">
      <span class="zy-log-at">${esc(entry.at)}</span>
      <span class="zy-log-target">[${esc(entry.target)}]</span>
      <span>${esc(entry.text)}</span></div>`;
  }

  function logMatches(entry) {
    if (logState.level && entry.level !== logState.level) return false;
    const text = logState.text.trim().toLowerCase();
    if (!text) return true;
    return (
      String(entry.text).toLowerCase().includes(text) ||
      String(entry.target).toLowerCase().includes(text)
    );
  }

  function paintLog() {
    const box = $("#log-box");
    if (!box) return;
    const shown = logState.lines.filter(logMatches);
    box.innerHTML = shown.length
      ? shown.map(logLine).join("")
      : `<div class="zy-log-line zy-log-debg">这一屏没有符合条件的行</div>`;
    if (logState.follow) box.scrollTop = box.scrollHeight;
  }

  function pushLog(entry) {
    logState.lines.push(entry);
    if (logState.lines.length > logState.limit) {
      logState.lines.splice(0, logState.lines.length - logState.limit);
    }
    const box = $("#log-box");
    if (!box || !logMatches(entry)) return;
    if (box.children.length === 1 && box.firstElementChild.classList.contains("zy-log-debg")) {
      box.innerHTML = "";
    }
    box.insertAdjacentHTML("beforeend", logLine(entry));
    while (box.children.length > logState.limit) box.removeChild(box.firstChild);
    if (logState.follow) box.scrollTop = box.scrollHeight;
  }

  async function pageLogs() {
    // 缓冲只增不改：回到这一页时不能把刚才推送过来的行又丢掉。
    if (!logState.loaded) {
      const data = await api("/logs");
      logState.lines = data.lines || [];
      logState.loaded = true;
    }
    const levels = [
      ["", "全部"],
      ["INFO", "INFO"],
      ["WARN", "WARN"],
      ["ERRO", "ERRO"],
      ["DEBG", "DEBG"],
    ];
    return `
      <div class="zy-search">
        ${ICONS.search}
        <input id="log-search" type="search" placeholder="按内容或 target 过滤"
               value="${esc(logState.text)}" aria-label="过滤日志">
      </div>
      <div class="zy-chips">
        ${levels
          .map(
            ([value, label]) => `<button class="zy-chip" type="button" data-level="${value}"
              aria-pressed="${logState.level === value}">${label}</button>`
          )
          .join("")}
        <button class="zy-chip" type="button" id="log-follow"
                aria-pressed="${logState.follow}">跟着滚</button>
        <button class="zy-chip" type="button" id="log-clear">清屏</button>
      </div>
      <div class="zy-log" id="log-box"></div>
      <p class="zy-note">这一屏只留最近 ${logState.limit} 行；完整的记录在
        <span class="zy-key">./bot logs</span> 那个窗口里。</p>`;
  }

  function mountLogs() {
    paintLog();
    if (logState.source) return;
    logState.source = new EventSource(`/api/logs/stream?t=${encodeURIComponent(token)}`);
    logState.source.onmessage = (event) => {
      try {
        pushLog(JSON.parse(event.data));
      } catch {
        /* 半行数据不该让整页掉线 */
      }
    };
    logState.source.onerror = () => {
      // 断开后浏览器自己会重连；接不上时不刷提示，免得盖住正在看的日志。
    };
  }

  /* ------------------------------ 页面：命令 ------------------------------ */

  const commandState = { output: "" };

  async function pageCommand() {
    const shortcuts = ["list", "diff ambient", "show oai", "defaults portrait"];
    return `
      <section class="zy-card">
        <div class="zy-section-title">本机控制台</div>
        <p class="zy-note">这里敲的和群里敲 <span class="zy-key">/ctl</span> 是同一套，
        以维护者身份执行。它只改这台机器上的配置，不往群里发消息——
        要给群里说话请回群里，或者用 agent 房间。</p>
        <form class="zy-field" id="command-form">
          <input class="zy-input zy-input-mono" id="command-input" autocomplete="off"
                 spellcheck="false" placeholder="list / show ambient / set oai …"
                 aria-label="控制命令">
          <button class="zy-btn zy-btn-filled" type="submit">${ICONS.play}执行</button>
        </form>
        <div class="zy-chips">
          ${shortcuts
            .map(
              (item) =>
                `<button class="zy-chip" type="button" data-run="${esc(item)}">${esc(
                  item
                )}</button>`
            )
            .join("")}
        </div>
      </section>
      <section class="zy-card">
        <div class="zy-section-title">回执</div>
        <div class="zy-code" id="command-output">${
          commandState.output ? esc(commandState.output) : "还没有执行过命令"
        }</div>
      </section>`;
  }

  async function runCommand(input) {
    const output = $("#command-output");
    if (output) output.textContent = "执行中…";
    try {
      const data = await api("/command", { method: "POST", body: { input } });
      commandState.output = data.message;
      if (output) output.textContent = data.message;
    } catch (error) {
      commandState.output = error.message;
      if (output) output.textContent = error.message;
      fail(error);
    }
  }

  /* ------------------------------ 事件 ------------------------------ */

  async function commitField(control, forced) {
    const path = control.dataset.path;
    const name = location.hash.replace(/^#\/plugins\//, "").split("/")[0];
    let value;
    try {
      value = control.dataset.kind === "bool" ? Boolean(forced) : parseValue(control.dataset.kind, control.value);
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

  function bindView() {
    const view = $("#view");

    view.addEventListener("click", async (event) => {
      const target = event.target;

      const go = target.closest("[data-nav]");
      if (go) {
        location.hash = `#/${go.dataset.nav}`;
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

      const drop = target.closest("[data-drop-bot]");
      if (drop) {
        if (!window.confirm(`删掉第 ${Number(drop.dataset.dropBot) + 1} 条连接？`)) return;
        try {
          const data = await api("/settings/bot", {
            method: "POST",
            body: { index: Number(drop.dataset.dropBot), remove: true, enabled: false, protocol: "satori", url: "" },
          });
          snack(data.message);
          render();
        } catch (error) {
          fail(error);
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

      const toggle = target.closest("[data-toggle]");
      if (toggle) {
        const next = toggle.getAttribute("aria-checked") !== "true";
        toggle.setAttribute("aria-checked", String(next));
        try {
          const data = await api(`/plugins/${encodeURIComponent(toggle.dataset.toggle)}/enabled`, {
            method: "POST",
            body: { on: next },
          });
          snack(data.message);
          render();
        } catch (error) {
          toggle.setAttribute("aria-checked", String(!next));
          fail(error);
        }
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
        render();
        return;
      }

      if (target.closest("#log-follow")) {
        logState.follow = !logState.follow;
        render();
        return;
      }

      if (target.closest("#log-clear")) {
        logState.lines = [];
        paintLog();
        return;
      }

      const reset = target.closest("[data-reset]");
      if (reset && window.confirm(reset.dataset.confirm)) {
        try {
          const data = await api(`/plugins/${encodeURIComponent(reset.dataset.reset)}/reset`, {
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
        const which = source.dataset.source;
        const box = $("#" + which);
        try {
          const data = await api("/ambient/source", {
            method: "POST",
            body: { name: which, text: box ? box.value : "" },
          });
          snack(data.message);
        } catch (error) {
          fail(error);
        }
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
        $("#command-input").value = shortcut.dataset.run;
        runCommand(shortcut.dataset.run);
        return;
      }

      const pluginRow = target.closest("[data-plugin]");
      if (pluginRow) location.hash = `#/plugins/${pluginRow.dataset.plugin}`;
    });

    view.addEventListener("change", async (event) => {
      const control = event.target.closest("[data-path]");
      if (!control || control.dataset.kind === "bool") return;
      await commitField(control);
    });

    view.addEventListener("keydown", async (event) => {
      if (event.key !== "Enter") return;
      const control = event.target.closest("input[data-path]");
      if (control) {
        event.preventDefault();
        await commitField(control);
      }
    });

    view.addEventListener("input", (event) => {
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

    const form = $("#command-form");
    if (form) {
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        const input = $("#command-input").value.trim();
        if (input) runCommand(input);
      });
    }

    // 设置页的两张表单：连接的每一条各一张，全局一张。都等按保存才提交，
    // 回车不提交（全局那张没有提交按钮，不拦下来会整页刷新）。
    view.addEventListener("submit", async (event) => {
      event.preventDefault();
      const botForm = event.target.closest("form[data-index]");
      if (!botForm) return;
      try {
        const data = await api("/settings/bot", { method: "POST", body: botPayload(botForm) });
        snack(data.message);
        render();
      } catch (error) {
        fail(error);
      }
    });
  }

  function bindLogScroll() {
    const box = $("#log-box");
    if (!box) return;
    box.addEventListener("scroll", () => {
      const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 48;
      if (atBottom === logState.follow) return;
      logState.follow = atBottom;
      const chip = $("#log-follow");
      if (chip) chip.setAttribute("aria-pressed", String(atBottom));
    });
  }

  /* ------------------------------ 路由 ------------------------------ */

  function currentRoute() {
    const hash = location.hash.replace(/^#\/?/, "");
    if (!hash) return { page: "overview", arg: null };
    const [head, ...rest] = hash.split("/");
    if (head === "plugins" && rest.length) {
      return { page: "plugins", arg: decodeURIComponent(rest.join("/")), detail: true };
    }
    if (head === "settings") return { page: "settings", arg: null };
    return PAGES.some((page) => page.id === head)
      ? { page: head, arg: null }
      : { page: "overview", arg: null };
  }

  async function render() {
    const route = currentRoute();
    renderNav(route.page);
    const view = $("#view");
    view.innerHTML = skeleton(route.detail ? 2 : 1);

    try {
      if (route.detail) {
        view.innerHTML = await pagePlugin(route.arg);
      } else {
        const paint = {
          overview: pageOverview,
          plugins: pagePlugins,
          ambient: pageAmbient,
          logs: pageLogs,
          command: pageCommand,
          settings: pageSettings,
        }[route.page];
        view.innerHTML = await paint();
      }
    } catch (error) {
      if (error && error.status === 503) {
        view.innerHTML = empty(error.message);
      } else if (error && error.status === 401) {
        renderLock();
        return;
      } else {
        view.innerHTML = empty(error && error.message ? error.message : "这一页没能读出来");
      }
      return;
    }

    if (route.detail) return;
    if (route.page === "overview") mountOverview();
    if (route.page === "logs") {
      mountLogs();
      bindLogScroll();
    }
  }

  /* ------------------------------ 启动 ------------------------------ */

  function boot() {
    applyTheme();
    $("#refresh").innerHTML = ICONS.refresh;
    $("#refresh").addEventListener("click", () => render());
    const gear = $("#settings");
    if (gear) {
      gear.innerHTML = ICONS.gear;
      gear.addEventListener("click", () => (location.hash = "#/settings"));
    }
    bindView();
    render();
  }

  window.addEventListener("hashchange", render);

  document.addEventListener("DOMContentLoaded", () => {
    if (!token) {
      renderLock();
      return;
    }
    boot();
  });
})();
