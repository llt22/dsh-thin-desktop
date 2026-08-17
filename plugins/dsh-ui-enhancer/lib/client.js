window.__ModuleLoader__.load({ id: "dsh-ui-enhancer", factory: (require) => { var module = { exports: {} }; var exports = module.exports;
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// src/client-entry.js
var client_entry_exports = {};
__export(client_entry_exports, {
  apply: () => apply,
  inject: () => inject
});
module.exports = __toCommonJS(client_entry_exports);
var import_react = __toESM(require("react"), 1);

// src/core.js
var THINKING_BLOCK = /^\s*<thinking>([\s\S]*?)<\/thinking>\s*$/;
function thinkingText(text) {
  if (typeof text !== "string") return void 0;
  const match = THINKING_BLOCK.exec(text);
  if (match === null) return void 0;
  const content = match[1].trim();
  return content === "" ? void 0 : content;
}
function enhanceAssistantNode(node) {
  const blocks = node?.data?.blocks;
  if (!Array.isArray(blocks)) return node;
  let changed = false;
  const enhanced = blocks.map((block) => {
    if (block?.kind !== "text") return block;
    const text = thinkingText(block.text);
    if (text === void 0) return block;
    changed = true;
    return { kind: "reasoning", text };
  });
  if (!changed) return node;
  return { ...node, data: { ...node.data, blocks: enhanced } };
}

// src/client-entry.js
var inject = ["slots", "sessions"];
function EnhancedAssistantNode(props) {
  const Original = EnhancedAssistantNode.original;
  const node = import_react.default.useMemo(() => enhanceAssistantNode(props.node), [props.node]);
  return import_react.default.createElement(
    import_react.default.Fragment,
    null,
    import_react.default.createElement(Original, { ...props, node })
  );
}
function registerAssistantRenderer(ctx) {
  let disposeRenderer;
  let disposeSubscription;
  const register = () => {
    if (disposeRenderer !== void 0) return;
    const official = ctx.slots.entries("conversation.chat.node").find((entry) => entry.options.key === "assistant-step" && (entry.options.priority ?? 0) === 0);
    if (official === void 0) return;
    EnhancedAssistantNode.original = official.component;
    disposeRenderer = ctx.slots.register({
      name: "conversation.chat.node",
      key: "assistant-step",
      priority: -10
    }, EnhancedAssistantNode);
    disposeSubscription?.();
    disposeSubscription = void 0;
  };
  disposeSubscription = ctx.slots.subscribe("conversation.chat.node", register);
  register();
  return () => {
    disposeSubscription?.();
    disposeRenderer?.();
  };
}
function registerEscapeCancellation(ctx) {
  const onKeyDown = (event) => {
    if (event.key !== "Escape" || event.repeat || event.defaultPrevented) return;
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
    const { current, byId } = ctx.sessions.list.getSnapshot();
    if (current === void 0 || byId[current]?.running !== true) return;
    const session = ctx.sessions.binding(current)?.session;
    if (session === void 0) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    void session.cancel().then((result) => {
      if (!result.ok) console.error("[dsh-ui-enhancer] failed to cancel generation:", result.error.message);
    }).catch((error) => {
      console.error("[dsh-ui-enhancer] failed to cancel generation:", error);
    });
  };
  document.addEventListener("keydown", onKeyDown, true);
  return () => document.removeEventListener("keydown", onKeyDown, true);
}
function apply(ctx) {
  ctx.effect(() => registerEscapeCancellation(ctx), "dsh-ui-enhancer: Escape cancellation");
  ctx.slots.inject("conversation.chat.node", () => registerAssistantRenderer(ctx));
}
return module.exports; } });
