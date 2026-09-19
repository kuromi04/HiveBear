import { NavLink } from "react-router-dom";
import { useState, useEffect } from "react";
import { motion } from "motion/react";
import {
  LayoutDashboard,
  Search,
  MessageSquare,
  Gauge,
  Network,
  Settings,
  UserCircle,
  PanelLeftClose,
  PanelLeft,
} from "lucide-react";

const primaryLinks = [
  { to: "/", label: "Dashboard", icon: LayoutDashboard },
  { to: "/models", label: "Models", icon: Search },
  { to: "/chat", label: "Chat", icon: MessageSquare },
];

const secondaryLinks = [
  { to: "/benchmark", label: "Benchmark", icon: Gauge },
  { to: "/mesh", label: "Mesh", icon: Network },
];

const bottomLinks = [
  { to: "/account", label: "Account", icon: UserCircle },
  { to: "/settings", label: "Settings", icon: Settings },
];

function useSidebarCollapsed() {
  const [collapsed, setCollapsed] = useState(() => {
    try { return localStorage.getItem("sidebar-collapsed") === "true"; } catch { return false; }
  });

  useEffect(() => {
    try { localStorage.setItem("sidebar-collapsed", String(collapsed)); } catch { /* ignore */ }
  }, [collapsed]);

  return [collapsed, setCollapsed] as const;
}

export default function Sidebar() {
  const [collapsed, setCollapsed] = useSidebarCollapsed();

  return (
    <motion.aside
      animate={{ width: collapsed ? 56 : 224 }}
      transition={{ type: "spring", damping: 30, stiffness: 400 }}
      className="flex h-full shrink-0 flex-col border-r border-border bg-surface overflow-hidden"
    >
      {/* Logo */}
      <div className="flex h-14 items-center gap-2 px-4">
        <img src="/assets/logo.png" alt="HiveBear" className="h-7 w-7 shrink-0 rounded-[var(--radius-md)]" />
        {!collapsed && (
          <div className="flex flex-col">
            <div className="flex items-baseline gap-1.5 whitespace-nowrap">
              <span className="text-sm font-bold tracking-tight text-text-primary">
                HiveBear
              </span>
              <span className="text-[10px] font-mono text-paw-500 font-semibold">
                by kuromi04
              </span>
            </div>
            <span className="text-[9px] text-text-muted">
              v0.2.0 • P2P Distributed
            </span>
          </div>
        )}
      </div>

      {/* Primary nav */}
      <nav className="flex flex-col gap-0.5 px-2 py-1">
        {primaryLinks.map((link) => (
          <SidebarLink key={link.to} {...link} collapsed={collapsed} />
        ))}
      </nav>

      {/* Separator */}
      <div className="mx-3 my-1.5 border-t border-border" />

      {/* Secondary nav */}
      <nav className="flex flex-col gap-0.5 px-2">
        {secondaryLinks.map((link) => (
          <SidebarLink key={link.to} {...link} collapsed={collapsed} />
        ))}
      </nav>

      {/* Spacer */}
      <div className="flex-1" />

      {/* Bottom nav */}
      <nav className="flex flex-col gap-0.5 px-2 pb-1">
        {bottomLinks.map((link) => (
          <SidebarLink key={link.to} {...link} collapsed={collapsed} />
        ))}
      </nav>

      {/* Collapse toggle + version */}
      <div className="flex items-center justify-between border-t border-border px-2 py-2">
        <button
          onClick={() => setCollapsed(!collapsed)}
          className="interactive-hover rounded-[var(--radius-md)] p-2 text-text-muted hover:bg-surface-overlay hover:text-text-primary"
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed ? <PanelLeft size={14} /> : <PanelLeftClose size={14} />}
        </button>
        {!collapsed && (
          <div className="flex flex-col items-end pr-1">
            <span className="text-[10px] text-text-muted">v{__APP_VERSION__}</span>
            <span className="text-[9px] text-paw-500/80 font-medium">Patched by @kuromi04</span>
          </div>
        )}
      </div>
    </motion.aside>
  );
}

function SidebarLink({
  to,
  label,
  icon: Icon,
  collapsed,
}: {
  to: string;
  label: string;
  icon: typeof LayoutDashboard;
  collapsed: boolean;
}) {
  return (
    <NavLink
      to={to}
      title={collapsed ? label : undefined}
      className={({ isActive }) =>
        [
          "group relative flex items-center gap-2.5 rounded-[var(--radius-md)] interactive-hover",
          collapsed ? "justify-center px-0 py-2" : "px-3 py-2",
          "text-sm",
          isActive
            ? "bg-surface-overlay text-text-primary"
            : "text-text-secondary hover:bg-surface-overlay/50 hover:text-text-primary",
        ].join(" ")
      }
    >
      {({ isActive }) => (
        <>
          {/* Active accent bar */}
          {isActive && (
            <motion.div
              layoutId="sidebar-active"
              className="absolute left-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-r-full bg-paw-500"
              transition={{ type: "spring", damping: 25, stiffness: 300 }}
            />
          )}
          <Icon size={16} strokeWidth={1.8} className="shrink-0" />
          {!collapsed && (
            <span className="truncate">{label}</span>
          )}
        </>
      )}
    </NavLink>
  );
}
