import process from "node:process";
import { bindings, defineConfig, exports } from "cf/config";
const name = "ttrs-connected";
export default defineConfig({
  worker: {
    name, workersDev: true, previewUrls: false, limits: { cpuMs: 5000 },
    entrypoint: "./src/index.ts", compatibilityDate: "2026-10-06", // Latest date supported by the selected pinned workerd.
    compatibilityFlags: ["nodejs_compat"],
    observability: { enabled: true, traces: { enabled: true }, logs: { enabled: true, invocationLogs: false } },
    exports: {
      WorkspaceDO: exports.durableObject({ storage: "sqlite" }),
      AuthorityDO: exports.durableObject({ storage: "sqlite" }),
    },
    env: {
      GRANTS: bindings.secret(),
      CONTROL: bindings.secret(),
      ORIGIN: bindings.text(process.env.TTRS_ORIGIN ?? "https://tt.invalid"),
      WORKSPACE: bindings.durableObject({ worker: name, exportName: "WorkspaceDO" }),
      AUTHORITY: bindings.durableObject({ worker: name, exportName: "AuthorityDO" }),
      BACKUPS: bindings.r2({ name: "ttrs-backups" }),
    },
  },
});
