/**
 * A simulated VS Code workbench: the slice of the `vscode` API the harness
 * extension uses (`vscode-harness-extension/harness.js`), modelling the state the
 * real built-ins change — the panel (visible, maximized, its terminal tabs, the
 * active one), the editor groups in grid order with their tabs, the active
 * group, the active terminal, the side bars — so the arrangement procedures run
 * end to end in Node.
 *
 * Behaviours copied from the pinned build (1.139.1), not invented:
 *   - `workbench.action.terminal.moveToEditor` moves the ACTIVE panel terminal
 *     into the ACTIVE editor group, and completes after the command resolves;
 *   - `workbench.action.terminal.moveToTerminalPanel` moves the active terminal
 *     editor to the panel; a group left empty closes (`closeEmptyGroups`);
 *   - the panel's maximized state follows `workbench.panel.opensMaximized`
 *     (`setPanelHidden` / `panelOpensMaximized` in the workbench bundle): hiding a
 *     maximized panel un-maximizes it but remembers it was maximized, and opening
 *     it re-maximizes under "always", or under "preserve" (the default) when it was
 *     last maximized, never under "never"; toggling a hidden panel opens it and
 *     leaves it maximized; the focus-nth-group commands address GRID order;
 *   - an unknown command rejects, as `executeCommand` does.
 *
 * `faults` breaks one behaviour so the harness's read-backs can be shown to
 * catch it.
 */

export function createFakeVscode({ version = "1.139.1", settings = {}, faults = {}, onQuit = () => {}, asyncMs = 3, firstPid = 51000 } = {}) {
  class TabInputTerminal {}
  const TerminalLocation = Object.freeze({ Panel: 1, Editor: 2 });
  const ViewColumn = Object.freeze({ Active: -1, Beside: -2, One: 1, Two: 2, Three: 3, Four: 4, Five: 5, Six: 6, Seven: 7, Eight: 8, Nine: 9 });
  const defaults = {
    "terminal.integrated.gpuAcceleration": "auto",
    "terminal.integrated.windowsUseConptyDll": true,
    "terminal.integrated.scrollback": 1000,
    "terminal.integrated.shellIntegration.enabled": true,
    "terminal.integrated.enablePersistentSessions": true,
    "workbench.panel.opensMaximized": "preserve",
  };
  const newGroup = () => ({ tabs: [], activeTab: null });
  const state = {
    panel: { visible: false, maximized: false, wasLastMaximized: false, terminals: [], active: null },
    groups: [newGroup()],
    activeGroup: 0,
    activeTerminal: null,
    sidebar: true,
    auxiliaryBar: true,
    commands: [],
    created: [],
    disposed: [],
    quit: false,
  };
  const closeListeners = [];
  let pid = firstPid;
  const later = (fn) => setTimeout(fn, asyncMs);

  const setting = (k) => (k in settings ? settings[k] : defaults[k]);
  /** Open the panel as the workbench does: maximized per opensMaximized. */
  function openPanel() {
    if (state.panel.visible) return;
    const mode = setting("workbench.panel.opensMaximized");
    state.panel.visible = true;
    state.panel.maximized = mode === "always" || (mode === "preserve" && state.panel.wasLastMaximized);
  }
  function hidePanel() {
    if (!state.panel.visible) return;
    state.panel.wasLastMaximized = state.panel.maximized;
    state.panel.maximized = false;
    state.panel.visible = false;
  }
  const groupIndexOf = (t) => state.groups.findIndex((g) => g.tabs.some((tab) => tab.terminal === t));
  const inPanel = (t) => state.panel.terminals.includes(t);

  function addTab(gi, t) {
    // `genericTabLabel`: a title template that does not show the terminal's name.
    const tab = { label: faults.genericTabLabel ?? t.name, input: new TabInputTerminal(), terminal: t };
    state.groups[gi].tabs.push(tab);
    state.groups[gi].activeTab = tab;
    return tab;
  }

  function removeTab(t) {
    const gi = groupIndexOf(t);
    if (gi < 0) return;
    const g = state.groups[gi];
    g.tabs = g.tabs.filter((tab) => tab.terminal !== t);
    g.activeTab = g.tabs[g.tabs.length - 1] ?? null;
    if (g.tabs.length === 0 && state.groups.length > 1) {
      state.groups.splice(gi, 1);
      if (state.activeGroup >= state.groups.length) state.activeGroup = state.groups.length - 1;
    }
  }

  function makeTerminal(options) {
    const t = {
      name: options.name,
      creationOptions: options,
      exitStatus: undefined,
      state: { isInteractedWith: false },
      shellIntegration: undefined,
      processId: Promise.resolve(pid++),
      show(preserveFocus) {
        t.lastShow = { preserveFocus };
        if (inPanel(t)) {
          openPanel();
          state.panel.active = t;
          state.activeTerminal = t;
          return;
        }
        const gi = groupIndexOf(t);
        if (gi >= 0) {
          state.groups[gi].activeTab = state.groups[gi].tabs.find((tab) => tab.terminal === t);
          state.activeGroup = gi;
          state.activeTerminal = t;
        }
      },
      hide() {},
      sendText() {},
      dispose() {
        if (state.disposed.includes(t)) return;
        state.disposed.push(t);
        state.panel.terminals = state.panel.terminals.filter((x) => x !== t);
        if (state.panel.active === t) state.panel.active = state.panel.terminals[0] ?? null;
        removeTab(t);
        if (state.activeTerminal === t) state.activeTerminal = state.panel.active ?? null;
        later(() => closeListeners.forEach((l) => l(t)));
      },
    };
    return t;
  }

  function leaves(node) {
    return Array.isArray(node.groups) && node.groups.length > 0 ? node.groups.reduce((n, g) => n + leaves(g), 0) : 1;
  }

  const commands = {
    async executeCommand(id, ...args) {
      state.commands.push(args.length ? [id, ...args] : id);
      const focusIdx = [
        "workbench.action.focusFirstEditorGroup",
        "workbench.action.focusSecondEditorGroup",
        "workbench.action.focusThirdEditorGroup",
        "workbench.action.focusFourthEditorGroup",
        "workbench.action.focusFifthEditorGroup",
        "workbench.action.focusSixthEditorGroup",
        "workbench.action.focusSeventhEditorGroup",
        "workbench.action.focusEighthEditorGroup",
      ].indexOf(id);
      if (focusIdx >= 0) {
        if (!faults.focusGroupNoop && focusIdx < state.groups.length) state.activeGroup = focusIdx;
        return undefined;
      }
      switch (id) {
        case "workbench.action.focusLastEditorGroup":
          state.activeGroup = state.groups.length - 1;
          return undefined;
        case "vscode.setEditorLayout": {
          const n = leaves(args[0]);
          const kept = state.groups.slice(0, n);
          const moved = state.groups.slice(n).flatMap((g) => g.tabs);
          while (kept.length < n) kept.push(newGroup());
          for (const tab of moved) {
            kept[n - 1].tabs.push(tab);
            kept[n - 1].activeTab = tab;
          }
          state.groups = kept;
          state.activeGroup = Math.min(state.activeGroup, n - 1);
          return undefined;
        }
        case "workbench.action.terminal.moveToEditor": {
          const t = state.panel.active;
          if (!t || faults.moveToEditorNoop) return undefined;
          const target = faults.moveToEditorWrongGroup ? 0 : state.activeGroup;
          later(() => {
            state.panel.terminals = state.panel.terminals.filter((x) => x !== t);
            state.panel.active = state.panel.terminals[0] ?? null;
            addTab(target, t);
            state.activeGroup = target;
            state.activeTerminal = t;
          });
          return undefined;
        }
        case "workbench.action.terminal.moveToTerminalPanel": {
          const t = state.activeTerminal;
          if (!t || groupIndexOf(t) < 0) return undefined;
          later(() => {
            removeTab(t);
            openPanel();
            state.panel.terminals.push(t);
            state.panel.active = t;
            state.activeTerminal = t;
          });
          return undefined;
        }
        case "workbench.action.closePanel":
          hidePanel();
          return undefined;
        case "workbench.action.toggleMaximizedPanel":
          if (state.panel.visible) {
            state.panel.maximized = !state.panel.maximized;
          } else {
            openPanel();
            state.panel.maximized = true;
          }
          state.panel.wasLastMaximized = state.panel.maximized;
          return undefined;
        case "workbench.action.closeSidebar":
          state.sidebar = false;
          return undefined;
        case "workbench.action.closeAuxiliaryBar":
          state.auxiliaryBar = false;
          return undefined;
        case "workbench.action.closeAllEditors":
          for (const g of state.groups) for (const tab of [...g.tabs]) tab.terminal.dispose();
          return undefined;
        case "workbench.action.quit":
          state.quit = true;
          onQuit();
          return undefined;
        default:
          throw new Error(`command '${id}' not found`);
      }
    },
  };

  const tabGroups = {
    get all() {
      return state.groups.map((g, i) => ({ viewColumn: i + 1, tabs: g.tabs, activeTab: g.activeTab, isActive: i === state.activeGroup }));
    },
    get activeTabGroup() {
      const g = state.groups[state.activeGroup];
      return g ? { viewColumn: state.activeGroup + 1, tabs: g.tabs, activeTab: g.activeTab, isActive: true } : undefined;
    },
  };

  const window = {
    createTerminal(options) {
      const t = makeTerminal(options);
      state.created.push(t);
      if (typeof options.location === "object" && options.location !== null) {
        const gi = options.location.viewColumn > 0 ? options.location.viewColumn - 1 : state.activeGroup;
        while (state.groups.length <= gi) state.groups.push(newGroup());
        addTab(gi, t);
      } else {
        // A new terminal becomes the active instance, so the panel's active tab.
        state.panel.terminals.push(t);
        state.panel.active = t;
      }
      state.activeTerminal = t;
      return t;
    },
    get terminals() {
      return state.created.filter((t) => !state.disposed.includes(t));
    },
    get activeTerminal() {
      return state.activeTerminal ?? undefined;
    },
    tabGroups,
    onDidCloseTerminal(listener) {
      closeListeners.push(listener);
      return { dispose: () => closeListeners.splice(closeListeners.indexOf(listener), 1) };
    },
  };

  const workspace = {
    getConfiguration(section) {
      const full = (k) => `${section}.${k}`;
      return {
        get: (k) => (full(k) in settings ? settings[full(k)] : defaults[full(k)]),
        inspect: (k) => ({ key: full(k), defaultValue: defaults[full(k)], globalValue: settings[full(k)] }),
      };
    },
  };

  return {
    vscode: { version, env: { appName: "Visual Studio Code", appHost: "desktop" }, window, commands, workspace, TabInputTerminal, TerminalLocation, ViewColumn },
    state,
    /** Where each live terminal is: `{panel: [names], groups: [[names]], active}`. */
    where() {
      return {
        panel: state.panel.terminals.map((t) => t.name),
        groups: state.groups.map((g) => g.tabs.map((tab) => tab.terminal.name)),
        active: state.activeTerminal?.name ?? null,
      };
    },
  };
}
