import { useEffect } from "react";
import { api, errorMessage, listenApiLog, listenEngineLog } from "./api";
import { useStore } from "./store";
import Sidebar from "./components/Sidebar";
import ChatPane from "./components/ChatPane";
import RightPanel from "./components/RightPanel";
import StatusBar from "./components/StatusBar";
import FlowView from "./components/FlowView";

/** Status is polled rather than pushed: a model load has no natural event, and
 * this is also what surfaces a crashed engine (the backend detects a dead child
 * on status read). One second is well under human patience and costs nothing. */
const STATUS_POLL_MS = 1000;

export default function App() {
  const setStatus = useStore((s) => s.setStatus);
  const setMetrics = useStore((s) => s.setMetrics);
  const setCaps = useStore((s) => s.setCaps);
  const setProbeError = useStore((s) => s.setProbeError);
  const setConfig = useStore((s) => s.setConfig);
  const pushLog = useStore((s) => s.pushLog);
  const setLogs = useStore((s) => s.setLogs);
  const pushApiLog = useStore((s) => s.pushApiLog);
  const flowOpen = useStore((s) => s.flowOpen);

  // One-time boot: find the engine, load defaults, start an empty chat.
  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const config = await api.defaultServeConfig();
        if (!cancelled) setConfig(config);
      } catch (e) {
        if (!cancelled) setProbeError(errorMessage(e));
      }

      try {
        const caps = await api.probeEngine();
        if (!cancelled) {
          setCaps(caps);
          setProbeError(null);
        }
      } catch (e) {
        // Not fatal: the user can still attach to a remote server, or point at
        // a binary by hand in Settings.
        if (!cancelled) {
          setCaps(null);
          setProbeError(errorMessage(e));
        }
      }

      try {
        const existing = await api.logs();
        if (!cancelled) setLogs(existing);
      } catch {
        /* logs are best-effort */
      }

      // Restore history, then open the most recent chat (or start one).
      const store = useStore.getState();
      await store.loadConversations();
      if (cancelled) return;
      const restored = useStore.getState().conversations;
      if (restored.length === 0) await store.newConversation();
      else await store.selectConversation(restored[0].id);

      try {
        const entries = await api.apiLog();
        if (!cancelled) useStore.getState().setApiLog(entries);
      } catch {
        /* api log is best-effort */
      }

      // Point the model browser at a sensible directory on first run.
      try {
        const root = await api.defaultModelsRoot();
        if (!cancelled && root) await useStore.getState().scanModels(root);
      } catch {
        /* the user can pick a directory by hand */
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [setCaps, setConfig, setLogs, setProbeError]);

  // Live engine output.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listenEngineLog(pushLog).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, [pushLog]);

  // Completed API calls, pushed rather than polled.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listenApiLog(pushApiLog).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, [pushApiLog]);

  // Connection status, and the server's counters while it is up. The counters
  // drive the status bar's streaming/resident state and token totals.
  useEffect(() => {
    let stop = false;
    const tick = async () => {
      try {
        const status = await api.status();
        if (stop) return;
        setStatus(status);
        setMetrics(status.kind === "ready" ? await api.metrics() : null);
      } catch {
        /* transient; the next tick retries */
      }
    };
    void tick();
    const id = setInterval(tick, STATUS_POLL_MS);
    return () => {
      stop = true;
      clearInterval(id);
    };
  }, [setStatus, setMetrics]);

  return (
    <div className={`app${flowOpen ? " flow-open" : ""}`}>
      <Sidebar />
      <ChatPane />
      <RightPanel />
      {flowOpen && (
        <section className="flow-panel">
          <FlowView />
        </section>
      )}
      <StatusBar />
    </div>
  );
}
