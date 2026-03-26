import { execSync } from "child_process";
import { existsSync } from "fs";
import { join } from "path";

let _targetDir: string | null = null;

/**
 * Resolves the cargo target directory, checking (in order):
 * 1. CARGO_TARGET_DIR env var
 * 2. `cargo metadata` output (handles .cargo/config.toml target-dir)
 * 3. Default ./target relative to workspace root
 */
function targetDir(): string {
  if (_targetDir) return _targetDir;

  if (process.env.CARGO_TARGET_DIR) {
    _targetDir = process.env.CARGO_TARGET_DIR;
    return _targetDir;
  }

  try {
    const meta = execSync("cargo metadata --format-version=1 --no-deps", {
      cwd: join(__dirname, "..", "..", ".."),
      encoding: "utf-8",
      timeout: 10_000,
    });
    const parsed = JSON.parse(meta);
    _targetDir = parsed.target_directory;
    return _targetDir!;
  } catch {
    // Fallback to default
    _targetDir = join(__dirname, "..", "..", "..", "target");
    return _targetDir;
  }
}

/**
 * Returns the path to a pre-built debug binary.
 * Throws if the binary does not exist.
 */
export function findBinary(name: string): string {
  const bin = join(targetDir(), "debug", name);
  if (!existsSync(bin)) {
    throw new Error(
      `Binary not found: ${bin}. Run 'cargo build -p ${name}' first.`
    );
  }
  return bin;
}

/**
 * Returns the path to a pre-built debug example binary.
 */
export function findExample(name: string): string {
  const bin = join(targetDir(), "debug", "examples", name);
  if (!existsSync(bin)) {
    throw new Error(
      `Example binary not found: ${bin}. Run 'cargo build --example ${name}' first.`
    );
  }
  return bin;
}
