const summary = document.querySelector("#build-summary");

async function showBuildInformation() {
  if (!(summary instanceof HTMLElement)) return;

  try {
    const response = await fetch("./build-info.json", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);

    const build = await response.json();
    const builtAt = new Date(build.builtAt);
    const formattedTime = new Intl.DateTimeFormat("zh-CN", {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(builtAt);

    summary.textContent = `最近构建：${formattedTime} · 提交 ${build.commit}`;
  } catch {
    summary.textContent = "当前页面提供最近一次成功构建的软件包";
  }
}

showBuildInformation();
