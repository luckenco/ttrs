// Generated cf bindings define the binding names and platform types.
// cf beta string entrypoints cannot infer RPC classes: narrow those two namespaces.
interface Env {
  WORKSPACE: DurableObjectNamespace<import("./workspace").WorkspaceDO>;
  AUTHORITY: DurableObjectNamespace<import("./authority").AuthorityDO>;
}
