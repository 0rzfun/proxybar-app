import { chmodSync, cpSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { prepareSingBox } from "./fetch-sing-box.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const platform = process.argv[2];
const tauri = resolve(root, "node_modules", ".bin", process.platform === "win32" ? "tauri.cmd" : "tauri");
const localMingwBin = resolve(root, "mingw64/bin");

if (process.platform === "win32" && existsSync(localMingwBin)) {
  process.env.Path = `${localMingwBin};${process.env.Path || ""}`;
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    env: process.env,
    shell: process.platform === "win32" && command.toLowerCase().endsWith(".cmd"),
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${basename(command)} exited with code ${result.status}`);
}

function commandOutput(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    env: process.env,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${basename(command)} exited with code ${result.status}`);
  return result.stdout.trim();
}

function verifyArchitectures(path, expected) {
  const actual = commandOutput("lipo", [path, "-archs"]).split(/\s+/).sort();
  const wanted = [...expected].sort();
  if (actual.join(" ") !== wanted.join(" ")) {
    throw new Error(`Unexpected architectures for ${path}: ${actual.join(" ")}`);
  }
}

function resetDirectory(path) {
  if (existsSync(path)) rmSync(path, { recursive: true, force: true });
  mkdirSync(path, { recursive: true });
}

function cleanGeneratedSchemas() {
  const schemas = resolve(root, "src-tauri/gen");
  if (existsSync(schemas)) rmSync(schemas, { recursive: true, force: true });
}

function findWebView2Loader(sourceDir) {
  const direct = resolve(sourceDir, "WebView2Loader.dll");
  if (existsSync(direct)) return direct;

  const buildDir = resolve(sourceDir, "build");
  if (!existsSync(buildDir)) return null;
  for (const name of readdirSync(buildDir)) {
    if (!name.startsWith("webview2-com-sys-")) continue;
    const candidate = resolve(buildDir, name, "out/x64/WebView2Loader.dll");
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

if (platform === "macos-x64" || platform === "macos-arm64") {
  if (process.platform !== "darwin") throw new Error("The macOS bundle must be built on macOS.");
  const arm64 = platform === "macos-arm64";
  prepareSingBox([platform]);
  const target = arm64 ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  const architecture = arm64 ? "arm64" : "x86_64";
  const label = arm64 ? "Apple-Silicon" : "Intel";
  run(tauri, ["build", "--target", target, "--bundles", "app"]);

  const source = resolve(root, ".build/target", target, "release/bundle/macos/ProxyBar.app");
  const outputDir = resolve(root, "release/macos");
  const output = join(outputDir, `ProxyBar-${label}.app`);
  if (!existsSync(source)) throw new Error(`Missing macOS bundle: ${source}`);

  resetDirectory(outputDir);
  cpSync(source, output, { recursive: true });
  const executable = join(output, "Contents/MacOS/proxybar");
  const bundledPlatform = join(output, "Contents/Resources/assets/platform");
  const unusedBinary = join(bundledPlatform, arm64 ? "sing-box-x86_64" : "sing-box-aarch64");
  if (existsSync(unusedBinary)) rmSync(unusedBinary);
  verifyArchitectures(executable, [architecture]);
  verifyArchitectures(join(bundledPlatform, arm64 ? "sing-box-aarch64" : "sing-box-x86_64"), [architecture]);
  run("codesign", ["--force", "--deep", "--sign", "-", output]);
  run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", output]);
  cleanGeneratedSchemas();
  console.log(`macOS ${label} release: ${output}`);
} else if (platform === "macos") {
  if (process.platform !== "darwin") throw new Error("The macOS bundle must be built on macOS.");
  prepareSingBox(["macos-x64", "macos-arm64"]);
  const target = "universal-apple-darwin";
  run(tauri, ["build", "--target", target, "--bundles", "app"]);

  const source = resolve(root, ".build/target", target, "release/bundle/macos/ProxyBar.app");
  const outputDir = resolve(root, "release/macos");
  if (!existsSync(source)) throw new Error(`Missing macOS bundle: ${source}`);

  const sourceExecutable = join(source, "Contents/MacOS/proxybar");
  const stagingDir = resolve(root, ".build/macos-packages");
  const intelExecutable = join(stagingDir, "proxybar-x86_64");
  const appleSiliconExecutable = join(stagingDir, "proxybar-arm64");
  resetDirectory(stagingDir);
  run("lipo", [sourceExecutable, "-thin", "x86_64", "-output", intelExecutable]);
  run("lipo", [sourceExecutable, "-thin", "arm64", "-output", appleSiliconExecutable]);

  resetDirectory(outputDir);
  const packages = [
    {
      output: join(outputDir, "ProxyBar-Intel.app"),
      executable: intelExecutable,
      architectures: ["x86_64"],
      keepSingBox: ["sing-box-x86_64"],
    },
    {
      output: join(outputDir, "ProxyBar-Apple-Silicon.app"),
      executable: appleSiliconExecutable,
      architectures: ["arm64"],
      keepSingBox: ["sing-box-aarch64"],
    },
    {
      output: join(outputDir, "ProxyBar-Universal.app"),
      executable: sourceExecutable,
      architectures: ["x86_64", "arm64"],
      keepSingBox: ["sing-box-x86_64", "sing-box-aarch64"],
    },
  ];

  for (const item of packages) {
    cpSync(source, item.output, { recursive: true });
    const executable = join(item.output, "Contents/MacOS/proxybar");
    const bundledPlatform = join(item.output, "Contents/Resources/assets/platform");
    if (item.executable !== sourceExecutable) {
      cpSync(item.executable, executable);
      chmodSync(executable, 0o755);
    }
    for (const binary of ["sing-box-x86_64", "sing-box-aarch64"]) {
      const path = join(bundledPlatform, binary);
      if (!item.keepSingBox.includes(binary) && existsSync(path)) rmSync(path);
    }
    verifyArchitectures(executable, item.architectures);
    for (const binary of item.keepSingBox) {
      verifyArchitectures(join(bundledPlatform, binary), [binary === "sing-box-x86_64" ? "x86_64" : "arm64"]);
    }
    run("codesign", ["--force", "--deep", "--sign", "-", item.output]);
    run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", item.output]);
    console.log(`macOS release: ${item.output}`);
  }
  rmSync(stagingDir, { recursive: true, force: true });
  cleanGeneratedSchemas();
} else if (platform === "windows") {
  if (process.platform !== "win32") throw new Error("The Windows portable app must be built on Windows.");
  prepareSingBox(["windows-x64"]);
  const target =
    process.env.WINDOWS_TARGET ||
    (existsSync(localMingwBin) || process.env.RUSTUP_TOOLCHAIN?.includes("windows-gnu")
      ? "x86_64-pc-windows-gnu"
      : "x86_64-pc-windows-msvc");
  if (target.endsWith("windows-gnu") && !process.env.RUSTUP_TOOLCHAIN) {
    process.env.RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu";
  }
  run(tauri, ["build", "--target", target, "--no-bundle"]);

  const sourceDir = resolve(root, ".build/target", target, "release");
  const sourceExe = resolve(sourceDir, "proxybar.exe");
  const sourceWebView2Loader = findWebView2Loader(sourceDir);
  const sourceAssets = resolve(sourceDir, "assets");
  const outputDir = resolve(root, "release/windows/portable");
  const output = join(outputDir, "ProxyBar.exe");
  if (!existsSync(sourceExe)) throw new Error(`Missing Windows executable: ${sourceExe}`);
  if (!sourceWebView2Loader) throw new Error(`Missing WebView2 loader in ${sourceDir}`);
  if (!existsSync(sourceAssets)) throw new Error(`Missing bundled assets: ${sourceAssets}`);
  mkdirSync(outputDir, { recursive: true });
  cpSync(sourceExe, output);
  cpSync(sourceWebView2Loader, join(outputDir, "WebView2Loader.dll"));
  cpSync(sourceAssets, join(outputDir, "assets"), { recursive: true });
  cleanGeneratedSchemas();
  console.log(`Windows portable release: ${outputDir}`);
} else {
  throw new Error("Usage: node scripts/build.mjs <macos|macos-x64|macos-arm64|windows>");
}
