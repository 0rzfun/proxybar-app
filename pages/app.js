const summary = document.querySelector("#build-summary");

async function showBuildInformation() {
  if (!(summary instanceof HTMLElement)) return;

  try {
    const response = await fetch("./build-info.json", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);

    const build = await response.json();
    const builtAt = new Date(build.builtAt);
    const packages = Array.isArray(build.packages) ? build.packages : [];
    const formattedTime = new Intl.DateTimeFormat("zh-CN", {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(builtAt);

    for (const button of document.querySelectorAll(".download-button[data-package]")) {
      if (!(button instanceof HTMLAnchorElement)) continue;
      if (packages.includes(button.dataset.package)) continue;
      button.removeAttribute("href");
      button.removeAttribute("download");
      button.setAttribute("aria-disabled", "true");
      button.classList.add("unavailable");
      button.textContent = "本次构建不可用";
    }

    summary.textContent = `最近构建：${formattedTime} · 提交 ${build.commit} · ${packages.length}/4 个软件包`;
  } catch {
    summary.textContent = "当前页面提供最近一次成功构建的软件包";
  }
}

showBuildInformation();
