/**
 * @vitest-environment jsdom
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { expect, it, vi } from "vitest";
import { UserMenu } from "./UserMenu";

vi.mock("@/lib/floatingMenu", () => ({
  FLOATING_MENU_Z_INDEX: 13_000,
  useFloatingMenu: () => ({
    pos: { top: 100, left: 10 },
    style: { position: "fixed" },
    settled: true,
  }),
}));

const labels = {
  settings: "Settings",
  theme: "Theme",
  themeSystem: "System",
  themeLight: "Light",
  themeDark: "Dark",
  local: "Local",
  signedIn: "Signed in",
  signedOut: "Signed out",
  login: "Log in",
  logout: "Log out",
  remaining: "remaining",
  profileActive: "Active",
  switchTo: "Switch to",
  customProvider: "Custom provider",
  resetsAt: "Resets",
};

function Harness({ collapsed }: { collapsed: boolean }) {
  const [open, setOpen] = useState(true);
  return (
    <>
      <output data-testid="open">{String(open)}</output>
      <UserMenu
        open={open}
        closeImmediately={collapsed}
        onClose={() => setOpen(false)}
        theme="dark"
        themePreference="dark"
        locale="en"
        labels={labels}
        account={null}
        activeProvider={null}
        accountBusy={false}
        onSettings={() => undefined}
        onAccountSettings={() => undefined}
        onTheme={() => undefined}
        onLogin={() => undefined}
        onLogout={() => undefined}
      >
        <button type="button">Account</button>
      </UserMenu>
    </>
  );
}

it("keeps sidebar search and drops the logo-adjacent update button", () => {
  const sidebar = readFileSync(
    resolve(__dirname, "../app/WorkbenchSidebar.tsx"),
    "utf8",
  );
  expect(sidebar).toContain('tr("sidebar.search")');
  expect(sidebar).toContain("onOpenSearch");
  expect(sidebar).not.toContain("SidebarUpdateButton");
  const menu = readFileSync(resolve(__dirname, "./UserMenu.tsx"), "utf8");
  expect(menu).not.toContain("onWhatsNew");
  expect(menu).not.toContain("whatsNew");
});

it("does not render a what's-new / 更新公告 entry", () => {
  const view = render(
    <UserMenu
      open
      onClose={() => undefined}
      theme="dark"
      themePreference="dark"
      locale="en"
      labels={labels}
      account={null}
      activeProvider={null}
      accountBusy={false}
      onSettings={() => undefined}
      onAccountSettings={() => undefined}
      onTheme={() => undefined}
      onLogin={() => undefined}
      onLogout={() => undefined}
    >
      <button type="button">Account</button>
    </UserMenu>,
  );
  expect(screen.queryByRole("menuitem", { name: /what.?s new|更新公告/i })).toBeNull();
  expect(screen.queryByText("What's new")).toBeNull();
  expect(screen.queryByText("更新公告")).toBeNull();
  view.unmount();
});

it("opens the theme editor from the theme submenu footer group", async () => {
  const onThemeEditor = vi.fn();
  const view = render(
    <UserMenu
      open
      onClose={() => undefined}
      theme="dark"
      themePreference="dark"
      locale="en"
      labels={{ ...labels, themeEditor: "Theme editor" }}
      account={null}
      activeProvider={null}
      accountBusy={false}
      onSettings={() => undefined}
      onAccountSettings={() => undefined}
      onTheme={() => undefined}
      onThemeEditor={onThemeEditor}
      onLogin={() => undefined}
      onLogout={() => undefined}
    >
      <button type="button">Account</button>
    </UserMenu>,
  );

  fireEvent.mouseEnter(screen.getByRole("menuitem", { name: "Theme" }));
  const editor = await screen.findByRole("menuitem", { name: "Theme editor" });
  expect(document.querySelector(".user-menu__flyout-sep")).not.toBeNull();
  fireEvent.click(editor);
  expect(onThemeEditor).toHaveBeenCalledTimes(1);
  view.unmount();
});

it("clears an open account menu when the sidebar collapses", async () => {
  const view = render(<Harness collapsed={false} />);
  expect(document.querySelector(".user-menu__pop--portal")).not.toBeNull();

  view.rerender(<Harness collapsed />);
  expect(document.querySelector(".user-menu__pop--portal")).toBeNull();
  await waitFor(() =>
    expect(screen.getByTestId("open").textContent).toBe("false"),
  );

  view.rerender(<Harness collapsed={false} />);
  expect(document.querySelector(".user-menu__pop--portal")).toBeNull();
});
