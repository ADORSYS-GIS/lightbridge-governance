// A minimal stand-in for the `vscode` module so the real provider code can be
// driven from a terminal. esbuild aliases 'vscode' to this at bundle time, so
// nothing in the repo knows this exists.
export const LanguageModelChatMessageRole = { User: 1, Assistant: 2 };
export const LanguageModelChatToolMode = { Auto: 1, Required: 2 };

export class LanguageModelTextPart {
  constructor(value) {
    this.value = value;
  }
}

export class LanguageModelToolCallPart {
  constructor(callId, name, input) {
    this.callId = callId;
    this.name = name;
    this.input = input;
  }
}

export class LanguageModelToolResultPart {
  constructor(callId, content) {
    this.callId = callId;
    this.content = content;
  }
}

export class LanguageModelDataPart {}

export class LanguageModelError extends Error {
  constructor(message, code) {
    super(message);
    this.code = code;
  }
  static NoPermissions(m) {
    return new LanguageModelError(m ?? '', 'NoPermissions');
  }
  static NotFound(m) {
    return new LanguageModelError(m ?? '', 'NotFound');
  }
  static Blocked(m) {
    return new LanguageModelError(m ?? '', 'Blocked');
  }
}

export class EventEmitter {
  constructor() {
    this.listeners = [];
    this.event = (fn) => {
      this.listeners.push(fn);
      return { dispose: () => {} };
    };
  }
  fire() {
    for (const fn of this.listeners) fn();
  }
  dispose() {}
}

// Settings the harness overrides before each scenario.
export const __settings = {};

const noop = () => ({ dispose: () => {} });

export const workspace = {
  getConfiguration: () => ({ get: (key) => __settings[key] }),
  onDidChangeConfiguration: noop,
};

export class MarkdownString {
  constructor(value) {
    this.value = value ?? '';
  }
}

export class ThemeColor {
  constructor(id) {
    this.id = id;
  }
}

export const StatusBarAlignment = { Left: 1, Right: 2 };

// What the harness inspects instead of an editor: every notification shown,
// every terminal command sent, and the status-bar item's rendered state. The
// point of the branch these support is that a signed-out developer is TOLD,
// so "was anything shown, and did it name the fix" is the assertion.
export const __ui = {
  errorMessages: [],
  /** Set by a scenario to pick an action button by label. */
  errorMessageResponse: undefined,
  terminals: [],
  statusBar: { visible: false, text: '', tooltip: '' },
};

export const window = {
  // Must mirror LogOutputChannel's full surface. A missing method here fails
  // a scenario for a reason that has nothing to do with the product — `debug`
  // was absent at first and presented as "sampling defaults" failing.
  createOutputChannel: () => ({
    trace: (m) => console.log(`  [log:trace] ${m}`),
    debug: (m) => console.log(`  [log:debug] ${m}`),
    info: (m) => console.log(`  [log:info] ${m}`),
    warn: (m) => console.log(`  [log:warn] ${m}`),
    error: (m) => console.log(`  [log:error] ${m}`),
    append: () => {},
    appendLine: () => {},
    clear: () => {},
    replace: () => {},
    show: () => {},
    hide: () => {},
    dispose: () => {},
  }),
  showQuickPick: async () => undefined,
  showInformationMessage: async () => undefined,
  showErrorMessage: async (message, ...actions) => {
    __ui.errorMessages.push({ message, actions });
    const wanted = __ui.errorMessageResponse;
    return actions.includes(wanted) ? wanted : undefined;
  },
  createTerminal: (name) => {
    const terminal = { name, shown: false, sent: [] };
    __ui.terminals.push(terminal);
    return {
      show: () => {
        terminal.shown = true;
      },
      sendText: (text, execute = true) => {
        terminal.sent.push({ text, execute });
      },
      dispose: () => {},
    };
  },
  createStatusBarItem: () => ({
    name: '',
    text: '',
    tooltip: undefined,
    command: undefined,
    backgroundColor: undefined,
    show() {
      __ui.statusBar = {
        visible: true,
        text: this.text,
        tooltip: this.tooltip?.value ?? String(this.tooltip ?? ''),
      };
    },
    hide() {
      __ui.statusBar = { visible: false, text: '', tooltip: '' };
    },
    dispose: () => {},
  }),
};

export const lm = { registerLanguageModelChatProvider: noop };

// Commands are registered into a real table so the suite can drive
// `lightbridge.signIn` exactly the way a notification button does — through
// `executeCommand`, not by importing the handler.
const registry = new Map();
export const commands = {
  registerCommand: (id, handler) => {
    registry.set(id, handler);
    return { dispose: () => registry.delete(id) };
  },
  executeCommand: async (id, ...args) => registry.get(id)?.(...args),
};
