import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const root = resolve(__dirname, "../..");

describe("Windows installer contract", () => {
  it("uses a stable NSIS identity and preserves installed user data", () => {
    const config = readFileSync(resolve(root, "electron-builder.yml"), "utf8");

    expect(config).toContain("appId: com.studypilot.desk");
    expect(config).toMatch(/target:\s*\n\s*-\s*target:\s*nsis/);
    expect(config).toContain("deleteAppDataOnUninstall: false");
    expect(config).toContain("from: build/backend-runtime/StudyPilotBackend");
    expect(config).toContain("to: backend");
    expect(config).not.toMatch(/from:\s*data\b/);
  });

  it("exposes one-command backend and installer builds", () => {
    const packageJson = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));

    expect(packageJson.scripts["build:backend-runtime"]).toBeTruthy();
    expect(packageJson.scripts["dist:win"]).toBeTruthy();
    expect(packageJson.devDependencies["electron-builder"]).toBeTruthy();
  });

  it("starts the packaged API from installer-provided environment", () => {
    const entry = readFileSync(resolve(root, "backend/packaged_server.py"), "utf8");

    expect(entry).toContain("STUDYPILOT_BACKEND_PORT");
    expect(entry).toContain("uvicorn.run");
  });

  it("ships a scalable icon source and a 1024px packager-compatible PNG", () => {
    const source = readFileSync(resolve(root, "build/icon.svg"), "utf8");
    const icon = readFileSync(resolve(root, "build/icon.png"));

    expect(source).toContain("<svg");
    expect(source).toContain("StudyPilot");
    expect(icon.subarray(1, 4).toString("ascii")).toBe("PNG");
    expect(icon.readUInt32BE(16)).toBe(1024);
    expect(icon.readUInt32BE(20)).toBe(1024);
  });
});
