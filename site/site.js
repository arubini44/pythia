// The main button downloads the package for the visitor's system, and that
// system's card is marked. build.py writes the newest release's links into
// the button's data attributes; without JavaScript it leads to #download.
(() => {
  "use strict";

  const NAMES = { windows: "Windows", mac: "macOS", linux: "Linux" };
  const KINDS = { windows: "Installer (.exe)", mac: "Disk image (.dmg)", linux: "AppImage" };

  function system() {
    const ua = navigator.userAgent || "";
    const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
    if (/Android|CrOS|iPhone|iPad/i.test(ua)) return null;
    if (/Win/i.test(platform) || /Windows/i.test(ua)) return "windows";
    if (/Mac/i.test(platform) || /Mac OS X/i.test(ua)) return "mac";
    if (/Linux/i.test(platform) || /Linux/i.test(ua)) return "linux";
    return null;
  }

  // Apple silicon unless the browser says Intel: Safari and Firefox don't
  // tell, and most Macs sold since 2021 are Apple silicon.
  async function macArch() {
    const data = navigator.userAgentData;
    if (data && data.getHighEntropyValues) {
      try {
        const v = await data.getHighEntropyValues(["architecture"]);
        if (v.architecture === "x86") return "x64";
      } catch (_) {
        /* keep the default */
      }
    }
    return "arm64";
  }

  async function main() {
    const os = system();
    if (!os) return;
    const card = document.querySelector(`.platform[data-os="${os}"]`);
    if (card) card.classList.add("is-yours");

    const button = document.getElementById("hero-download");
    const note = document.getElementById("hero-note");
    if (!button) return;
    let key = os;
    let kind = KINDS[os];
    if (os === "mac") {
      const arch = await macArch();
      key = `mac-${arch}`;
      kind = `${kind}, ${arch === "arm64" ? "Apple silicon" : "Intel"}`;
    }
    const url = button.dataset[camel(key)];
    if (!url) return;
    button.href = url;
    button.textContent = `Download for ${NAMES[os]}`;
    if (note) {
      const size = button.dataset[camel(`${key}-size`)];
      const version = note.dataset.version || "";
      note.replaceChildren(
        `${kind} · ${size} · Version ${version} · `,
        link("#download", os === "mac" ? "Intel Mac or other systems" : "Other systems"),
      );
    }
  }

  function camel(name) {
    return name.replace(/-(\w)/g, (_, c) => c.toUpperCase());
  }

  function link(href, text) {
    const a = document.createElement("a");
    a.href = href;
    a.textContent = text;
    return a;
  }

  // A link to a question of the FAQ opens it.
  function openTarget() {
    const el = location.hash && document.getElementById(location.hash.slice(1));
    if (el && el.tagName === "DETAILS") el.open = true;
  }

  addEventListener("hashchange", openTarget);
  openTarget();
  main();
})();
