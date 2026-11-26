(() => {
  const root = document.documentElement;

  /* ---------- 主题切换：Auto → Light → Dark ---------- */
  const modes = ["system", "light", "dark"];
  const labels = { system: "Auto", light: "Light", dark: "Dark" };
  const mq = window.matchMedia("(prefers-color-scheme: dark)");

  function applyTheme(mode) {
    const theme = mode === "system" ? (mq.matches ? "dark" : "light") : mode;
    root.dataset.theme = theme;
    root.dataset.themeMode = mode;
    document.querySelectorAll("[data-theme-label]").forEach((el) => (el.textContent = labels[mode]));
  }
  applyTheme(root.dataset.themeMode || "system");
  mq.addEventListener("change", () => {
    if (root.dataset.themeMode === "system") applyTheme("system");
  });
  document.querySelectorAll("[data-theme-toggle]").forEach((btn) =>
    btn.addEventListener("click", () => {
      const next = modes[(modes.indexOf(root.dataset.themeMode) + 1) % modes.length];
      try { localStorage.setItem("hya:theme", next); } catch (e) {}
      applyTheme(next);
    })
  );

  /* ---------- 标签切换框 + 复制 ---------- */
  document.querySelectorAll("[data-tabs]").forEach((box) => {
    const tabs = [...box.querySelectorAll("[data-tab]")];
    const panels = [...box.querySelectorAll("[data-panel]")];
    const indicator = box.querySelector(".tab-indicator");
    const copyBtn = box.querySelector("[data-copy]");

    function moveIndicator(tab) {
      if (!indicator) return;
      indicator.style.setProperty("--x", tab.offsetLeft + "px");
      indicator.style.setProperty("--w", tab.offsetWidth + "px");
    }
    function select(tab) {
      tabs.forEach((t) => {
        const on = t === tab;
        t.classList.toggle("is-active", on);
        t.setAttribute("aria-selected", on);
      });
      panels.forEach((p) => (p.hidden = p.dataset.panel !== tab.dataset.tab));
      moveIndicator(tab);
    }
    tabs.forEach((t) => t.addEventListener("click", () => select(t)));
    const active = tabs.find((t) => t.classList.contains("is-active")) || tabs[0];
    requestAnimationFrame(() => moveIndicator(active));
    window.addEventListener("resize", () => moveIndicator(box.querySelector(".tab.is-active")));

    copyBtn?.addEventListener("click", async () => {
      const text = panels.find((p) => !p.hidden)?.textContent.trim() || "";
      try {
        await navigator.clipboard.writeText(text);
        copyBtn.textContent = "Copied";
      } catch (e) {
        copyBtn.textContent = "Failed";
      }
      setTimeout(() => (copyBtn.textContent = "Copy"), 1400);
    });
  });

  /* ---------- Hero logo：点击重播动画 ---------- */
  const heroLogo = document.querySelector(".hero-logo");
  heroLogo?.addEventListener("click", () => {
    heroLogo.classList.add("is-replaying");
    void heroLogo.getBoundingClientRect();
    heroLogo.classList.remove("is-replaying");
  });

  /* ---------- Story：章节滚动联动左侧展示区 ---------- */
  const sections = [...document.querySelectorAll(".story-sections .section")];
  const sectionsWrap = document.querySelector(".story-sections");
  const stageMedia = document.querySelector("[data-stage-media]");
  const stageTitle = document.querySelector("[data-stage-title]");
  const stageCaption = document.querySelector("[data-stage-caption]");
  const defaultTitle = stageTitle?.textContent;

  // 为移动端在每个章节标题下生成内联展示框
  sections.forEach((sec) => {
    const tpl = sec.querySelector("template.section-media");
    if (!tpl) return;
    const fig = document.createElement("figure");
    fig.className = "figure-frame section-inline-media";
    fig.innerHTML = `<figcaption class="figure-caption"><span></span><span class="live-dot"></span></figcaption><div class="media-frame"></div><div class="media-caption"></div>`;
    fig.querySelector(".figure-caption span").textContent = sec.dataset.title || "";
    fig.querySelector(".media-frame").append(tpl.content.cloneNode(true));
    const cap = sec.querySelector("template.section-caption");
    if (cap) fig.querySelector(".media-caption").textContent = cap.content.textContent.trim();
    sec.querySelector(".section-title")?.after(fig);
  });

  let activeSection = null;
  function setActive(sec) {
    if (sec === activeSection) return;
    activeSection = sec;
    sections.forEach((s) => s.classList.toggle("is-active", s === sec));
    if (!stageMedia || !sec) return;
    const tpl = sec.querySelector("template.section-media");
    if (tpl) {
      const wrap = document.createElement("div");
      wrap.className = "media-content";
      wrap.append(tpl.content.cloneNode(true));
      stageMedia.replaceChildren(wrap);
    }
    if (stageTitle) stageTitle.textContent = sec.dataset.title || defaultTitle;
    const cap = sec.querySelector("template.section-caption");
    if (stageCaption) stageCaption.textContent = cap ? cap.content.textContent.trim() : "";
  }

  if (sections.length) {
    sectionsWrap.classList.add("has-scroll-sync");
    const onScroll = () => {
      const line = window.innerHeight * 0.4;
      let current = null;
      for (const s of sections) {
        if (s.getBoundingClientRect().top <= line) current = s;
      }
      // 还没滚到第一个章节时，第一个章节保持高亮但不替换默认展示
      if (!current) {
        sections.forEach((s, i) => s.classList.toggle("is-active", i === 0));
        return;
      }
      setActive(current);
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
  }

  /* ---------- 文档页：目录随滚动高亮 ---------- */
  const docLinks = [...document.querySelectorAll(".docs-nav-list a[href^='#']")];
  if (docLinks.length) {
    const targets = docLinks
      .map((a) => document.getElementById(a.getAttribute("href").slice(1)))
      .filter(Boolean);
    const onDocScroll = () => {
      const line = window.innerHeight * 0.3;
      let current = targets[0];
      for (const t of targets) if (t.getBoundingClientRect().top <= line) current = t;
      // 滚到页面底部时高亮最后一节
      if (window.innerHeight + window.scrollY >= document.body.scrollHeight - 2) current = targets[targets.length - 1];
      docLinks.forEach((a) => a.classList.toggle("is-active", a.getAttribute("href") === "#" + current?.id));
    };
    window.addEventListener("scroll", onDocScroll, { passive: true });
    onDocScroll();
  }

  /* ---------- 卡片滚动渐入 ---------- */
  const io = new IntersectionObserver(
    (entries) =>
      entries.forEach((e) => {
        if (e.isIntersecting) {
          e.target.classList.add("is-visible");
          io.unobserve(e.target);
        }
      }),
    { threshold: 0.15 }
  );
  document.querySelectorAll(".reveal").forEach((el, i) => {
    el.style.transitionDelay = (i % 3) * 80 + "ms";
    io.observe(el);
  });
})();
