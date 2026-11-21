import { NavLink } from "react-router-dom";
import {
  LayoutDashboard,
  MessageSquare,
  Search,
  Settings,
  UserCircle,
} from "lucide-react";

const tabs = [
  { to: "/", label: "Home", icon: LayoutDashboard },
  { to: "/models", label: "Models", icon: Search },
  { to: "/chat", label: "Chat", icon: MessageSquare },
  { to: "/account", label: "Account", icon: UserCircle },
  { to: "/settings", label: "Settings", icon: Settings },
];

export default function BottomTabs() {
  return (
    <nav className="shrink-0 z-40 flex min-h-[52px] items-center border-t border-border bg-surface select-none pt-1 pb-[env(safe-area-inset-bottom,4px)]">
      {tabs.map((tab) => (
        <NavLink
          key={tab.to}
          to={tab.to}
          className={({ isActive }) =>
            [
              "flex flex-1 flex-col items-center justify-center gap-0.5 py-1.5 text-[10px]",
              isActive
                ? "text-paw-500 font-medium"
                : "text-text-muted hover:text-text-secondary active:text-text-secondary",
            ].join(" ")
          }
        >
          {({ isActive }) => (
            <>
              <tab.icon
                size={20}
                strokeWidth={isActive ? 2 : 1.5}
              />
              <span>{tab.label}</span>
            </>
          )}
        </NavLink>
      ))}
    </nav>
  );
}
