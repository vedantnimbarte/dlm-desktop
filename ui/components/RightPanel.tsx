import { useStore, type RightPanel as Tab } from "../store";
import ModelTab from "./ModelTab";
import SettingsTab from "./SettingsTab";
import LogsTab from "./LogsTab";

const TABS: Array<{ id: Tab; label: string }> = [
  { id: "model", label: "Model" },
  { id: "settings", label: "Settings" },
  { id: "logs", label: "Logs" },
];

export default function RightPanel() {
  const active = useStore((s) => s.rightPanel);
  const setActive = useStore((s) => s.setRightPanel);

  return (
    <aside className="panel">
      <div className="tabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={`tab${t.id === active ? " active" : ""}`}
            onClick={() => setActive(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div className="panel-body">
        {active === "model" && <ModelTab />}
        {active === "settings" && <SettingsTab />}
        {active === "logs" && <LogsTab />}
      </div>
    </aside>
  );
}
