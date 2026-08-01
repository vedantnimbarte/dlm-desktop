import { useEffect } from "react";
import { api, errorMessage, listenEngineLog } from "./api";
import { useStore } from "./store";
import Sidebar from "./components/Sidebar";
import ChatPane from "./components/ChatPane";
import RightPanel from "./components/RightPanel";
import StatusBar from "./components/StatusBar";

/** Status is polled rather than pushed: a model load has no natural event, and
 * this is also what surfaces a crashed engine (the backend detects a dead child
 * on status read). One second is well under human patience and costs nothing. */
const STATUS_POLL_MS = 1000;

export default function App() {
  const setStatus = useStore((s) => s.setStatus);
  const setCaps = useStore((s) => s.setCaps);
  const setProbeError = useStore((s) => s.setProbeError);
  const setConfig = useStore((s) => s.setConfig);
  const pushLog = useStore((s) => s.pushLog);
  const setLogs = useStore((s) => s.setLogs);
  const newConversation = useStore((s) => s.newConversation);

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
    })();

    if (useStore.getState().conversations.length === 0) newConversation();

    return () => {
      cancelled = true;
    };
  }, [setCaps, setConfig, setLogs, setProbeError, newConversation]);

  // Live engine output.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listenEngineLog(pushLog).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, [pushLog]);

  // Connection status.
  useEffect(() => {
    let stop = false;
    const tick = async () => {
      try {
        const status = await api.status();
        if (!stop) setStatus(status);
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
  }, [setStatus]);

  return (
    <div className="app">
      <Sidebar />
      <ChatPane />
      <RightPanel />
      <StatusBar />
    </div>
  );
}
