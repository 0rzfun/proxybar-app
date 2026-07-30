import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

export const SING_BOX_VERSION = "1.13.15";

const artifacts = {
  "macos-x64": {
    archive: `sing-box-${SING_BOX_VERSION}-darwin-amd64-legacy-macos-10.13.tar.gz`,
    sha256: "2347e0b6d860f1cbae04f9c3d0db6cfaed38273020578870a80eb4e616800cb1",
    directory: `sing-box-${SING_BOX_VERSION}-darwin-amd64-legacy-macos-10.13`,
    files: [["sing-box", "assets/platforms/macos/sing-box-x86_64", 0o755]],
  },
  "macos-arm64": {
    archive: `sing-box-${SING_BOX_VERSION}-darwin-arm64.tar.gz`,
    sha256: "3452d866834c9572389e5ca73e60d4ee45a7d5b79332188c9a9e533c5fd40a6d",
    directory: `sing-box-${SING_BOX_VERSION}-darwin-arm64`,
    files: [["sing-box", "assets/platforms/macos/sing-box-aarch64", 0o755]],
  },
  "windows-x64": {
    archive: `sing-box-${SING_BOX_VERSION}-windows-amd64.zip`,
    sha256: "599b296f6e57511d36d2a6f3011aed1a86fa98418578bbb06bd6dc241b5d8877",
    directory: `sing-box-${SING_BOX_VERSION}-windows-amd64`,
    files: [
      ["sing-box.exe", "assets/platforms/windows/sing-box.exe", 0o755],
      ["libcronet.dll", "assets/platforms/windows/libcronet.dll", 0o644],
    ],
  },
};

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    env: process.env,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${basename(command)} exited with code ${result.status}`);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function hostTarget() {
  if (process.platform === "darwin" && process.arch === "x64") return "macos-x64";
  if (process.platform === "darwin" && process.arch === "arm64") return "macos-arm64";
  if (process.platform === "win32" && process.arch === "x64") return "windows-x64";
  throw new Error(`Unsupported sing-box host: ${process.platform}/${process.arch}`);
}

function download(artifact, archivePath) {
  const url = `https://github.com/SagerNet/sing-box/releases/download/v${SING_BOX_VERSION}/${artifact.archive}`;
  const args = ["-L", "--fail", "--silent", "--show-error", "--retry", "3"];
  if (process.env.SING_BOX_DOWNLOAD_PROXY) {
    args.push("--proxy", process.env.SING_BOX_DOWNLOAD_PROXY);
  }
  args.push("--output", archivePath, url);
  console.log(`Downloading sing-box ${SING_BOX_VERSION}: ${artifact.archive}`);
  run("curl", args);
}

function prepareTarget(target) {
  const artifact = artifacts[target];
  if (!artifact) throw new Error(`Unknown sing-box target: ${target}`);

  const downloadDirectory = resolve(root, ".build/sing-box-downloads", SING_BOX_VERSION);
  const extractDirectory = resolve(root, ".build/sing-box-extract", target);
  const archivePath = join(downloadDirectory, artifact.archive);
  mkdirSync(downloadDirectory, { recursive: true });

  if (existsSync(archivePath) && sha256(archivePath) !== artifact.sha256) {
    rmSync(archivePath);
  }
  if (!existsSync(archivePath)) download(artifact, archivePath);

  const actualHash = sha256(archivePath);
  if (actualHash !== artifact.sha256) {
    rmSync(archivePath);
    throw new Error(
      `SHA-256 mismatch for ${artifact.archive}: expected ${artifact.sha256}, received ${actualHash}`,
    );
  }

  if (existsSync(extractDirectory)) rmSync(extractDirectory, { recursive: true });
  mkdirSync(extractDirectory, { recursive: true });
  run("tar", ["-xf", archivePath, "-C", extractDirectory]);

  const sourceDirectory = join(extractDirectory, artifact.directory);
  for (const [sourceName, destinationName, mode] of artifact.files) {
    const source = join(sourceDirectory, sourceName);
    const destination = resolve(root, destinationName);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(source, destination);
    chmodSync(destination, mode);
  }

  const license = resolve(root, "assets/common/licenses/sing-box-LICENSE.txt");
  mkdirSync(dirname(license), { recursive: true });
  copyFileSync(join(sourceDirectory, "LICENSE"), license);
  chmodSync(license, 0o644);
  console.log(`Prepared sing-box ${SING_BOX_VERSION} for ${target}`);
}

export function prepareSingBox(requestedTargets) {
  const targets = requestedTargets.map((target) => (target === "host" ? hostTarget() : target));
  for (const target of [...new Set(targets)]) prepareTarget(target);
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  const requestedTargets = process.argv.slice(2);
  if (requestedTargets.length === 0) {
    throw new Error("Usage: node scripts/fetch-sing-box.mjs <host|macos-x64|macos-arm64|windows-x64> [...]");
  }
  prepareSingBox(requestedTargets);
}
