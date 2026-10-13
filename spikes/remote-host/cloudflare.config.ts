import { bindings, defineConfig, exports } from "cf/config";

const name = "ttrs-remote-host-lab";
export default defineConfig(({ mode }) => ({
  worker: {
    name,
    entrypoint: mode === "fallback" ? "./fallback/host.ts" : "./build/index.js",
    compatibilityDate: "2026-10-06",
    exports: {
      Workspace: exports.durableObject({ storage: "sqlite" }),
      Authority: exports.durableObject({ storage: "sqlite" }),
    },
    env: {
      // Deliberately public, throwaway lab credentials; never deploy this lab.
      LAB_READ_TOKEN: bindings.text("lab-read-only"),
      LAB_READ_ROTATED_TOKEN: bindings.text("lab-read-rotated"),
      LAB_WRITE_TOKEN: bindings.text("lab-write-only"),
      LAB_BACKUPS: bindings.r2({ name: "ttrs-remote-host-lab" }),
      WORKSPACE: bindings.durableObject({ worker: name, exportName: "Workspace" }),
      AUTHORITY: bindings.durableObject({ worker: name, exportName: "Authority" }),
    },
  },
}));
