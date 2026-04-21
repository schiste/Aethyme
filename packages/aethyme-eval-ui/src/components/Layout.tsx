import { NavLink, Outlet } from "react-router-dom";

const tabs = [
  { to: "/results", label: "Results" },
  { to: "/matrix", label: "Matrix" },
  { to: "/charts", label: "Charts" },
  { to: "/repositories", label: "Repositories" },
  { to: "/run", label: "Run Evals" },
] as const;

export default function Layout() {
  return (
    <div className="min-h-screen flex flex-col">
      {/* Header */}
      <header className="border-b border-[var(--color-border)] bg-[var(--color-surface)]">
        <div className="max-w-7xl mx-auto px-6 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <h1 className="text-lg font-semibold tracking-tight text-[var(--color-text)]">
              Aethyme Eval Dashboard
            </h1>
            <span className="text-xs font-mono px-2 py-0.5 rounded bg-[var(--color-accent)]/15 text-[var(--color-accent)]">
              local
            </span>
          </div>
        </div>

        {/* Tab navigation */}
        <nav className="max-w-7xl mx-auto px-6 flex gap-1">
          {tabs.map((tab) => (
            <NavLink
              key={tab.to}
              to={tab.to}
              className={({ isActive }) =>
                [
                  "px-4 py-2.5 text-sm font-medium rounded-t-lg transition-colors",
                  isActive
                    ? "bg-[var(--color-bg)] text-[var(--color-text)] border border-b-0 border-[var(--color-border)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-surface-hover)]",
                ].join(" ")
              }
            >
              {tab.label}
            </NavLink>
          ))}
        </nav>
      </header>

      {/* Main content */}
      <main className="flex-1 max-w-7xl w-full mx-auto px-6 py-6">
        <Outlet />
      </main>
    </div>
  );
}
