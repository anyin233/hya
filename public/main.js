(() => {
  const root = document.documentElement;
  const metaScheme = document.querySelector('meta[name="color-scheme"]');

  /* ---------- 1. i18n (zh ⇄ en) & Theme (<meta name="color-scheme"> 驱动) ---------- */
  const modes = ["system", "light", "dark"];
  const mqDark = window.matchMedia("(prefers-color-scheme: dark)");

  const themeLabels = {
    zh: {
      systemLight: "跟随系统 · 浅色",
      systemDark: "跟随系统 · 深色",
      light: "浅色",
      dark: "深色",
    },
    en: {
      systemLight: "System · Light",
      systemDark: "System · Dark",
      light: "Light",
      dark: "Dark",
    },
  };
  const copyFeedback = {
    zh: { ok: "已复制", err: "复制失败" },
    en: { ok: "Copied", err: "Copy failed" },
  };

  const currentLang = () => (root.dataset.lang === "en" ? "en" : "zh");

  function currentThemeMode() {
    const c = metaScheme?.content;
    return c === "light" || c === "dark" ? c : "system";
  }

  function syncThemeLabels() {
    const lang = currentLang();
    const mode = currentThemeMode();
    const key = mode === "system" ? (mqDark.matches ? "systemDark" : "systemLight") : mode;
    const text = themeLabels[lang][key];
    document.querySelectorAll("[data-theme-label]").forEach((el) => {
      el.textContent = text;
    });
  }

  function applyThemeMode(mode) {
    const next = mode === "light" || mode === "dark" ? mode : "system";
    if (metaScheme) {
      metaScheme.content = next === "system" ? "light dark" : next;
    }
    syncThemeLabels();
  }

  let activeSection = null;
  const stageTitle = document.querySelector("[data-stage-title]");

  function applyLang(lang) {
    const next = lang === "en" ? "en" : "zh";
    root.dataset.lang = next;
    root.lang = next === "zh" ? "zh-CN" : "en";

    const pageTitle = next === "en" ? root.dataset.titleEn : root.dataset.titleZh;
    if (pageTitle) document.title = pageTitle;

    const metaDesc = document.querySelector('meta[name="description"]');
    const desc = next === "en" ? metaDesc?.dataset.descEn : metaDesc?.dataset.descZh;
    if (metaDesc && desc) metaDesc.setAttribute("content", desc);

    document.querySelectorAll("[data-aria-zh]").forEach((el) => {
      const label = next === "en" ? el.dataset.ariaEn : el.dataset.ariaZh;
      if (label) el.setAttribute("aria-label", label);
    });

    document.querySelectorAll("[data-lang-option]").forEach((item) => {
      item.setAttribute("aria-checked", String(item.dataset.langOption === next));
    });

    syncThemeLabels();
  }

  syncThemeLabels();
  mqDark.addEventListener("change", syncThemeLabels);

  document.querySelectorAll("[data-theme-toggle]").forEach((btn) =>
    btn.addEventListener("click", () => {
      const next = modes[(modes.indexOf(currentThemeMode()) + 1) % modes.length];
      try {
        if (next === "system") localStorage.removeItem("hya:theme");
        else localStorage.setItem("hya:theme", next);
      } catch {}
      applyThemeMode(next);
    })
  );

  /* ---------- 1b. 语言菜单（Popover API + Invoker / popovertarget 回退 + focusgroup="menu"） ---------- */
  const supportsFocusGroup = "focusGroup" in HTMLElement.prototype;
  const supportsCommandFor = "commandForElement" in HTMLButtonElement.prototype;
  const supportsPositionArea = CSS.supports("position-area: block-end");

  if (!supportsFocusGroup) {
    document.querySelectorAll('[focusgroup~="menu"]').forEach((group) => {
      group.setAttribute("role", "menu");
      group.querySelectorAll("button:not([role])").forEach((item) => {
        item.setAttribute("role", "menuitem");
      });
      group.addEventListener(
        "keydown",
        (event) => {
          if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
          const items = [...group.querySelectorAll("button")]
            .filter((item) => item.closest("[focusgroup]") === group)
            .filter((item) => !item.disabled && item.getAttribute("aria-disabled") !== "true");
          const current = items.indexOf(document.activeElement);
          if (current < 0) return;
          const nextIdx =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? items.length - 1
                : (current + (event.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
          event.preventDefault();
          event.stopImmediatePropagation();
          items[nextIdx]?.focus({ preventScroll: true });
        },
        true
      );
    });
  }

  const langTrigger = document.getElementById("lang-trigger");
  const langMenu = document.getElementById("lang-menu");

  if (langTrigger && langMenu) {
    if (!supportsCommandFor) {
      langTrigger.setAttribute("popovertarget", "lang-menu");
    }

    const enterOnOpen = new WeakSet();
    let pointerPressed = false;
    const menuItems = [...langMenu.querySelectorAll("[data-lang-option]")];

    const isMenuOpen = () =>
      langMenu.matches(":popover-open") || langMenu.classList.contains("is-open-fallback");

    const closeLangMenu = (restoreFocus = false) => {
      if (typeof langMenu.hidePopover === "function") {
        if (langMenu.matches(":popover-open")) langMenu.hidePopover();
      } else {
        langMenu.classList.remove("is-open-fallback");
        langMenu.hidden = true;
        langTrigger.setAttribute("aria-expanded", "false");
      }
      if (restoreFocus) langTrigger.focus({ preventScroll: true });
    };

    const positionMenuFallback = () => {
      if (supportsPositionArea) return;
      const rect = langTrigger.getBoundingClientRect();
      const menuW = langMenu.offsetWidth || 150;
      const menuH = langMenu.offsetHeight || 84;
      const pad = 8;
      let left = rect.right - menuW;
      left = Math.max(pad, Math.min(left, window.innerWidth - menuW - pad));
      let top = rect.bottom + 6;
      if (top + menuH > window.innerHeight - pad && rect.top - menuH - 6 >= pad) {
        top = rect.top - menuH - 6;
      }
      langMenu.style.left = `${left}px`;
      langMenu.style.top = `${top}px`;
    };

    langTrigger.addEventListener("pointerdown", () => {
      pointerPressed = true;
    });

    langTrigger.addEventListener("click", () => {
      if (!pointerPressed) enterOnOpen.add(langMenu);
      pointerPressed = false;
      if (typeof langMenu.showPopover !== "function") {
        const nextOpen = !isMenuOpen();
        langMenu.hidden = !nextOpen;
        langMenu.classList.toggle("is-open-fallback", nextOpen);
        langTrigger.setAttribute("aria-expanded", String(nextOpen));
        if (nextOpen) {
          positionMenuFallback();
          if (enterOnOpen.has(langMenu)) {
            enterOnOpen.delete(langMenu);
            (menuItems.find((i) => i.getAttribute("aria-checked") === "true") || menuItems[0])?.focus({
              preventScroll: true,
            });
          }
        }
      }
    });

    langTrigger.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        if (isMenuOpen()) {
          (menuItems.find((i) => i.getAttribute("aria-checked") === "true") || menuItems[0])?.focus({
            preventScroll: true,
          });
        } else {
          enterOnOpen.add(langMenu);
          pointerPressed = false;
          langTrigger.click();
        }
      }
    });

    langMenu.addEventListener("toggle", (e) => {
      const open = e.newState === "open";
      langTrigger.setAttribute("aria-expanded", String(open));
      if (open) {
        positionMenuFallback();
        if (enterOnOpen.has(langMenu)) {
          enterOnOpen.delete(langMenu);
          (menuItems.find((i) => i.getAttribute("aria-checked") === "true") || menuItems[0])?.focus({
            preventScroll: true,
          });
        }
      }
    });

    langMenu.addEventListener("keydown", (e) => {
      if (e.key === "Tab") {
        closeLangMenu(false);
      } else if (e.key === "Escape" && typeof langMenu.hidePopover !== "function") {
        e.preventDefault();
        closeLangMenu(true);
      }
    });

    document.addEventListener("focusin", (e) => {
      if (isMenuOpen() && !langMenu.contains(e.target) && !langTrigger.contains(e.target)) {
        closeLangMenu(false);
      }
    });

    menuItems.forEach((item) => {
      item.addEventListener("pointermove", () => {
        item.focus({ preventScroll: true });
      });
      item.addEventListener("click", () => {
        const chosen = item.dataset.langOption === "en" ? "en" : "zh";
        try {
          localStorage.setItem("hya:lang", chosen);
        } catch {}
        applyLang(chosen);
        const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
        setTimeout(() => closeLangMenu(true), reduceMotion ? 0 : 140);
      });
    });
  }

  window.addEventListener("storage", (e) => {
    if (e.key === "hya:theme") {
      applyThemeMode(e.newValue === "light" || e.newValue === "dark" ? e.newValue : "system");
    } else if (e.key === "hya:lang" && (e.newValue === "en" || e.newValue === "zh")) {
      applyLang(e.newValue);
    }
  });

  /* ---------- 2. 安装选项卡（WAI-ARIA Roving Tabindex / focusgroup + hidden="until-found"） ---------- */
  document.querySelectorAll("[data-tabs]").forEach((box) => {
    const tabs = [...box.querySelectorAll('[role="tab"][data-tab]')];
    const panels = [...box.querySelectorAll('[role="tabpanel"][data-panel]')];
    const copyBtn = box.querySelector("[data-copy]");

    function select(tab, moveFocus = false) {
      if (!tab) return;
      tabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        t.setAttribute("tabindex", on ? "0" : "-1");
        t.toggleAttribute("focusgroupstart", on);
      });
      panels.forEach((p) => {
        const on = p.dataset.panel === tab.dataset.tab;
        if (on) {
          p.removeAttribute("hidden");
          p.setAttribute("tabindex", "0");
          const codeEl = p.querySelector("code[id]");
          if (copyBtn && codeEl) copyBtn.setAttribute("commandfor", codeEl.id);
        } else {
          p.setAttribute("hidden", "until-found");
          p.removeAttribute("tabindex");
        }
      });
      if (moveFocus) tab.focus();
      tab.scrollIntoView({ block: "nearest", inline: "nearest" });
    }

    tabs.forEach((tab, idx) => {
      tab.addEventListener("click", () => select(tab));
      if (!supportsFocusGroup) {
        tab.addEventListener("keydown", (e) => {
          let nextIdx = -1;
          if (e.key === "ArrowRight") nextIdx = (idx + 1) % tabs.length;
          else if (e.key === "ArrowLeft") nextIdx = (idx - 1 + tabs.length) % tabs.length;
          else if (e.key === "Home") nextIdx = 0;
          else if (e.key === "End") nextIdx = tabs.length - 1;
          if (nextIdx >= 0) {
            e.preventDefault();
            select(tabs[nextIdx], true);
          }
        });
      }
    });

    box.addEventListener("beforematch", (e) => {
      const panel = e.target.closest("[data-panel]");
      if (!panel) return;
      const matchingTab = tabs.find((t) => t.dataset.tab === panel.dataset.panel);
      if (matchingTab) select(matchingTab);
    });
  });

  /* ---------- 3. Invoker Commands 复制按钮 ---------- */
  const copyTimers = new WeakMap();
  let lastCopyAnnouncement = "";

  function reportCopy(button, ok) {
    const tip = button.nextElementSibling;
    clearTimeout(copyTimers.get(button));
    button.removeAttribute("data-copied");
    button.removeAttribute("data-failed");

    let message = copyFeedback[currentLang()][ok ? "ok" : "err"];
    if (message === lastCopyAnnouncement) message += "\u00A0";
    lastCopyAnnouncement = message;

    if (tip) tip.textContent = "";
    void button.offsetWidth;
    button.setAttribute(ok ? "data-copied" : "data-failed", "");
    if (tip) requestAnimationFrame(() => (tip.textContent = message));

    copyTimers.set(
      button,
      setTimeout(() => {
        button.removeAttribute("data-copied");
        button.removeAttribute("data-failed");
        if (tip) tip.textContent = "";
      }, 1800)
    );
  }

  async function copyTarget(button, target) {
    const text = target?.innerText?.trim() || target?.textContent?.trim() || "";
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      reportCopy(button, true);
    } catch {
      reportCopy(button, false);
    }
  }

  if ("CommandEvent" in window) {
    document.addEventListener(
      "command",
      (event) => {
        if (event.command === "--copy") {
          copyTarget(event.source, event.target);
        }
      },
      true
    );
  } else {
    document.addEventListener("click", (event) => {
      if (event.defaultPrevented) return;
      const button = event.target.closest('button[command="--copy"]');
      if (!button) return;
      const target = document.getElementById(button.getAttribute("commandfor") || "");
      if (target) copyTarget(button, target);
    });
  }

  /* ---------- 4. Hero Logo：点击重播入场动画 ---------- */
  const heroLogo = document.querySelector(".hero-logo");
  heroLogo?.addEventListener("click", () => {
    heroLogo.classList.add("is-replaying");
    void heroLogo.getBoundingClientRect();
    heroLogo.classList.remove("is-replaying");
  });

  /* ---------- 5. Story：章节滚动联动左侧真实 Hya TUI 展示台 ---------- */
  const sections = [...document.querySelectorAll(".story-sections .section")];
  const sectionsWrap = document.querySelector(".story-sections");
  const stageMedia = document.querySelector("[data-stage-media]");

  sections.forEach((sec) => {
    const tpl = sec.querySelector("template.section-media");
    if (!tpl) return;
    const fig = document.createElement("figure");
    fig.className = "figure-frame section-inline-media";

    const capBar = document.createElement("figcaption");
    capBar.className = "figure-caption";
    const capLeft = document.createElement("div");
    capLeft.className = "figure-caption-left";
    const dots = document.createElement("span");
    dots.className = "win-dots";
    dots.setAttribute("aria-hidden", "true");
    dots.append(document.createElement("i"), document.createElement("i"), document.createElement("i"));
    const titleSpan = document.createElement("span");
    titleSpan.className = "stage-title-text";
    titleSpan.textContent = sec.dataset.title || "hya";
    capLeft.append(dots, titleSpan);
    capBar.append(capLeft);

    const frame = document.createElement("div");
    frame.className = "media-frame";
    frame.append(tpl.content.cloneNode(true));

    fig.append(capBar, frame);
    sec.querySelector(".section-title")?.after(fig);
  });

  let programmaticUntil = 0;

  document.querySelectorAll('a[href^="#"]').forEach((link) => {
    link.addEventListener("click", () => {
      root.classList.add("is-scrolled");
      const id = link.getAttribute("href").slice(1);
      const target = sections.find((s) => s.id === id);
      if (target) {
        programmaticUntil = Date.now() + 750;
        setActive(target);
      }
    });
  });

  function setActive(sec) {
    if (!sec || sec === activeSection) return;
    activeSection = sec;
    sections.forEach((s) => {
      s.classList.toggle("is-active", s === sec);
    });
    if (!stageMedia) return;
    const tpl = sec.querySelector("template.section-media");
    if (tpl) {
      const wrap = document.createElement("div");
      wrap.className = "media-content";
      wrap.append(tpl.content.cloneNode(true));
      stageMedia.replaceChildren(wrap);
    }
    if (stageTitle) stageTitle.textContent = sec.dataset.title || "hya";
  }

  if (sections.length) {
    const storyGrid = document.querySelector(".story-grid");
    root.classList.add("has-hero-fold");
    sectionsWrap.classList.add("has-scroll-sync");

    const syncFold = () => {
      const entered =
        window.scrollY > 24 ||
        Boolean(window.location.hash) ||
        (storyGrid ? storyGrid.getBoundingClientRect().top <= window.innerHeight * 0.88 : false);
      root.classList.toggle("is-scrolled", entered);
    };

    const onScroll = () => {
      syncFold();
      if (Date.now() < programmaticUntil) return;
      const mid = window.innerHeight * 0.5;
      let current = sections[0];
      let bestDist = Infinity;
      for (const s of sections) {
        const rect = s.getBoundingClientRect();
        const dist = Math.abs((rect.top + rect.bottom) * 0.5 - mid);
        if (dist < bestDist) {
          bestDist = dist;
          current = s;
        }
      }
      setActive(current);
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", syncFold, { passive: true });
    onScroll();
  }

  applyLang(currentLang());
})();
