import {
  type ClipboardEvent,
  type DragEvent,
  type KeyboardEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  type AgentEntry,
  agentColdAt,
  agentEntries,
  goToAgent,
  recentAgents,
  workingLabel,
} from "@/agents";
import { attachmentName, attachmentsField, uploadFile } from "@/attachments";
import type { Client } from "@/client";
import {
  CacheColdError,
  HeldGoneError,
  type ColdChoice,
  type ColdChoiceMade,
  archivedAction,
  coldAction,
  coldPanelShown,
  coldSpell,
  rewarmField,
} from "@/cold";
import {
  type CommandCard,
  type OwnCommand,
  type PanelEffect,
  type PanelEvent,
  type PanelState,
  closedPanel,
  commandCards,
  commandQuery,
  filterCards,
  panelStep,
} from "@/commands";
import { afterDismissal, complete, drawerFor } from "@/completion";
import { AgentsPanel } from "@/components/AgentsPanel";
import { AskCard, type SubmitAnswer } from "@/components/AskCard";
import { type ComposerFile, ComposerFiles } from "@/components/Attachments";
import {
  AgentColdPanel,
  type CacheDeadline,
  CacheLowBanner,
  ColdPanel,
  cacheDeadline,
  useCacheGauge,
} from "@/components/ColdPanel";
import { CommandField, CommandsPanel } from "@/components/CommandsPanel";
import { usePermissionMode } from "@/components/PermissionMode";
import { type FileLinks, FileLinksContext } from "@/components/Markdown";
import { DesktopSend, TouchSend } from "@/components/SendControls";
import { SlashDrawer } from "@/components/SlashDrawer";
import {
  type MenuEntry,
  type MenuState,
  MenuPills,
  SessionMenu,
  closedMenu,
} from "@/components/SessionMenu";
import { RestartBanner } from "@/components/RestartBanner";
import { SearchButton } from "@/components/HistorySearch";
import { Header, HomeButton, SessionColumn } from "@/components/TopBar";
import { type HeldActions, type PendingSend, TranscriptView } from "@/components/TranscriptView";
import { useSnapshot } from "@/components/useSnapshot";
import { useSeeCooling } from "@/coolingSeen";
import {
  DiffIcon,
  FileIcon,
  FolderIcon,
  PaperclipIcon,
  navigateBackLeaves,
} from "@/components/Explorer";
import {
  changeExplorer,
  openFileInLastFree,
  openFolderInLastFree,
  openSessionDiff,
  openSessionFile,
  openSessionFolder,
  openSessionFolderAt,
} from "@/files";
import { repoRoot } from "@/git";
import { folderName, sessionTitle, stateLabel } from "@/format";
import { navigate } from "@/route";
import type { SendAction } from "@/sendGesture";
import {
  DEFAULT_MODEL,
  type Settings,
  type SettingsFlow,
  appliedOutcome,
  closedFlow,
  defaultEffort,
  effortLevels,
  effortTabLabel,
  firstButton,
  modelOptionLabel,
  modelPillLabel,
  modelTabLabel,
  pickEffort,
  pickModel,
  settingsPlan,
  shownEffort,
} from "@/settings";
import {
  type TranscriptState,
  askContext,
  emptyTranscript,
  parseTime,
  wakeText,
} from "@/transcript";
import type { HeldMessage, HeldMode } from "@/wire";

/** Unsent composer text per discussion (`draftKey`), kept while the app stays open. */
const drafts = new Map<string, string>();
/** Files attached to the unsent draft per discussion, uploads still running included. */
const draftFiles = new Map<string, ComposerFile[]>();

/** Paths of a draft's uploaded files. */
function readyPaths(slot: string): string[] {
  return (draftFiles.get(slot) ?? []).flatMap((file) =>
    file.status === "ready" && file.path !== null ? [file.path] : [],
  );
}

/** Sets a draft's text and uploaded files, replacing what it held. */
function fillDraft(slot: string, text: string, paths: readonly string[]): void {
  if (text === "") drafts.delete(slot);
  else drafts.set(slot, text);
  const files = paths.map((path): ComposerFile => {
    nextFileKey += 1;
    return {
      key: String(nextFileKey),
      name: attachmentName(path),
      status: "ready",
      path,
      error: null,
    };
  });
  updateDraftFiles(slot, () => files);
}

/** Changes a draft's files, whether or not its composer is on screen. */
function updateDraftFiles(
  slot: string,
  change: (files: ComposerFile[]) => ComposerFile[],
): ComposerFile[] {
  const next = change(draftFiles.get(slot) ?? []);
  if (next.length === 0) draftFiles.delete(slot);
  else draftFiles.set(slot, next);
  return next;
}

let nextFileKey = 0;
/** Per session, the last cold-cache choice and the spell it was made for. */
const coldChoices = new Map<string, ColdChoiceMade>();
/** Per session, the agent whose discussion is on screen; absent for the main one. */
const viewed = new Map<string, string>();

/** The draft slot of a session's main discussion, or of one of its agents. */
function draftKey(session: string, agent: string | null): string {
  return agent === null ? session : `${session}:${agent}`;
}

/** A send waiting for its `user_sent` or `relay_sent`, in the discussion it was typed in. */
type ScopedSend = PendingSend & { scope: string | null };

/** Spread into a `send` or a `relay`. */
type SendFields = { rewarm?: true; attachments?: string[] };

/** Client ids of the messages the server announced, in every discussion of the session. */
function sentClientIds(transcript: TranscriptState): Set<string> {
  const ids = new Set<string>();
  for (const state of [transcript, ...transcript.scopes.values()]) {
    for (const item of state.items) {
      if (item.kind === "user" && item.clientId !== null) ids.add(item.clientId);
    }
  }
  return ids;
}

function coarsePointer(): boolean {
  return window.matchMedia("(pointer: coarse)").matches;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function Composer({
  session,
  uploadTo,
  placeholder,
  turnOpen,
  stopping,
  menuOpen,
  commands,
  recentCommands,
  onSuggest,
  onSend,
  onStop,
  onMenu,
  onMenuToggle,
  takeFocus,
  deadline,
}: {
  /** The draft slot of the discussion on screen. */
  session: string;
  /** The session whose uploads folder takes the attached files. */
  uploadTo: string;
  placeholder: string;
  turnOpen: boolean;
  /** A stop was sent and awaits the server. */
  stopping: boolean;
  menuOpen: boolean;
  /** What a `/word` completes to. */
  commands: readonly CommandCard[];
  /** Command names run lately, newest first: what a bare `/` suggests first. */
  recentCommands: readonly string[];
  /** A `/word` opened the suggestions. */
  onSuggest: () => void;
  onSend: (text: string, attachments: string[]) => void;
  onStop: () => void;
  /** Opens the menu (swipe up on touch). */
  onMenu: () => void;
  /** Opens or closes the menu (the desktop button). */
  onMenuToggle: () => void;
  /** True when the input takes the focus on mount. */
  takeFocus: () => boolean;
  /** Null outside the main discussion or without a warm cache: no low-cache warning. */
  deadline: CacheDeadline | null;
}) {
  const [text, setText] = useState(() => drafts.get(session) ?? "");
  const [files, setFiles] = useState<ComposerFile[]>(() => draftFiles.get(session) ?? []);
  const area = useRef<HTMLTextAreaElement>(null);
  const measure = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    if (takeFocus()) area.current?.focus();
  }, []);

  // An upload may end after the composer left the screen: the draft's files still change.
  const changeFiles = (change: (files: ComposerFile[]) => ComposerFile[]) => {
    const next = updateDraftFiles(session, change);
    if (mounted.current) setFiles(next);
  };
  const settle = (key: string, patch: Partial<ComposerFile>) =>
    changeFiles((list) => list.map((file) => (file.key === key ? { ...file, ...patch } : file)));

  const attach = (picked: readonly File[]) => {
    for (const file of picked) {
      nextFileKey += 1;
      const key = String(nextFileKey);
      const name = file.name === "" ? "pasted" : file.name;
      changeFiles((list) => [...list, { key, name, status: "uploading", path: null, error: null }]);
      uploadFile(uploadTo, file).then(
        (path) => settle(key, { status: "ready", path }),
        (err: unknown) => settle(key, { status: "failed", error: errorMessage(err) }),
      );
    }
  };
  const remove = (key: string) => changeFiles((list) => list.filter((file) => file.key !== key));

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const pasted = Array.from(event.clipboardData.files);
    if (pasted.length === 0) return;
    event.preventDefault();
    attach(pasted);
  };
  const carriesFiles = (event: DragEvent) => event.dataTransfer.types.includes("Files");
  const onDragOver = (event: DragEvent<HTMLDivElement>) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "copy";
  };
  const onDrop = (event: DragEvent<HTMLDivElement>) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    attach(Array.from(event.dataTransfer.files));
  };

  // Measured on an out-of-flow copy: collapsing the input itself would resize
  // the transcript for a moment and move its scroll position.
  useLayoutEffect(() => {
    const element = area.current;
    const copy = measure.current;
    if (element === null || copy === null) return;
    element.style.height = `${copy.scrollHeight}px`;
  }, [text]);

  const change = (value: string) => {
    setText(value);
    if (value === "") drafts.delete(session);
    else drafts.set(session, value);
  };

  // Null while a range is selected or the input is not focused.
  const [caret, setCaret] = useState<number | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [selection, setSelection] = useState({ key: "", index: 0 });
  const pendingCaret = useRef<number | null>(null);
  const readCaret = (element: HTMLTextAreaElement) =>
    setCaret(element.selectionStart === element.selectionEnd ? element.selectionStart : null);
  useLayoutEffect(() => {
    const element = area.current;
    const at = pendingCaret.current;
    if (element === null || at === null) return;
    pendingCaret.current = null;
    element.setSelectionRange(at, at);
  }, [text]);

  const shown = afterDismissal(drawerFor(text, caret, commands, recentCommands), dismissed);
  if (shown.dismissed !== dismissed) setDismissed(shown.dismissed);
  const { drawer } = shown;
  const suggesting = drawer.kind === "suggest" ? drawer.key : null;
  useEffect(() => {
    if (suggesting !== null) onSuggest();
  }, [suggesting]);
  const selectionKey = drawer.kind === "suggest" ? `${drawer.key}:${drawer.word.query}` : "";
  const selected = selection.key === selectionKey ? selection.index : 0;
  const pick = (card: CommandCard) => {
    if (drawer.kind !== "suggest") return;
    const next = complete(text, drawer.word, card.name);
    setCaret(next.caret);
    if (next.text === text) {
      area.current?.setSelectionRange(next.caret, next.caret);
      return;
    }
    pendingCaret.current = next.caret;
    change(next.text);
  };
  const dismiss = () => {
    if (drawer.kind !== "none") setDismissed(drawer.key);
  };

  const uploading = files.some((file) => file.status === "uploading");
  const paths = readyPaths(session);
  const ready = !uploading && (text.trim() !== "" || paths.length > 0);

  const submit = () => {
    if (!ready) return;
    onSend(text, paths);
    change("");
    changeFiles(() => []);
    // On touch, dropping focus closes the virtual keyboard so the reply is visible.
    if (coarsePointer()) area.current?.blur();
  };

  // Desktop: Enter sends, Shift+Enter inserts a newline. Touch: Enter inserts a
  // newline and only the send button sends. An IME composition keeps its own Enter.
  // With the drawer open: arrows move, Tab or Enter completes (Enter sends a word already
  // complete), Escape dismisses.
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.nativeEvent.isComposing) return;
    if (drawer.kind !== "none" && event.key === "Escape") {
      event.preventDefault();
      dismiss();
      return;
    }
    if (drawer.kind === "suggest" && !coarsePointer()) {
      const count = drawer.suggestions.length;
      const step = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
      if (step !== 0) {
        event.preventDefault();
        setSelection({ key: selectionKey, index: (selected + step + count) % count });
        return;
      }
      const card = drawer.suggestions[selected]?.card;
      const completes =
        (event.key === "Tab" || event.key === "Enter") &&
        !event.shiftKey &&
        card !== undefined &&
        (event.key === "Tab" || card.name !== drawer.word.query);
      if (completes) {
        event.preventDefault();
        pick(card);
        return;
      }
    }
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing &&
      !coarsePointer()
    ) {
      event.preventDefault();
      submit();
    }
  };

  const coarse = coarsePointer();
  // Under the files, the text starts closer to them.
  const padding = files.length === 0 ? "py-4" : "pt-2 pb-4";
  const onAction = (action: SendAction) => {
    switch (action) {
      case "send":
        submit();
        return;
      case "stop":
        onStop();
        return;
      case "menu":
        onMenu();
        return;
    }
  };

  // The buttons keep a one-line input's height and sit at the bottom as the input grows.
  const row = (
    <div
      onDragOver={onDragOver}
      onDrop={onDrop}
      className="mx-auto flex max-w-5xl items-end gap-2 select-text"
    >
      <input
        ref={picker}
        type="file"
        multiple
        hidden
        onChange={(event) => {
          attach(Array.from(event.target.files ?? []));
          event.target.value = "";
        }}
      />
      <button
        type="button"
        aria-label="Attach files"
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => picker.current?.click()}
        className="flex h-14 w-10 shrink-0 cursor-pointer items-center justify-center rounded-[5px] border bg-card text-muted-foreground hover:bg-muted"
      >
        <PaperclipIcon />
      </button>
      <div className="relative flex min-w-0 flex-1 flex-col rounded-[5px] border bg-card focus-within:ring-2 focus-within:ring-ring">
        {drawer.kind !== "none" && (
          <SlashDrawer drawer={drawer} selected={selected} onPick={pick} onDismiss={dismiss} />
        )}
        <ComposerFiles files={files} onRemove={remove} />
        <div className="relative flex min-w-0 flex-1">
          <textarea
            ref={measure}
            rows={1}
            value={text}
            readOnly
            tabIndex={-1}
            aria-hidden
            className={`${COMPOSER_INPUT} ${padding} pointer-events-none invisible absolute inset-x-0 bottom-0`}
          />
          <textarea
            ref={area}
            rows={1}
            value={text}
            onChange={(event) => {
              change(event.target.value);
              readCaret(event.target);
            }}
            onSelect={(event) => readCaret(event.currentTarget)}
            onBlur={() => setCaret(null)}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            enterKeyHint={coarse ? "enter" : "send"}
            placeholder={placeholder}
            className={`${COMPOSER_INPUT} ${padding} max-h-[35dvh] flex-1 outline-none`}
          />
        </div>
      </div>
      {coarse ? (
        <TouchSend
          ready={ready}
          stoppable={turnOpen && !stopping}
          stopping={stopping}
          onAction={onAction}
        />
      ) : (
        <DesktopSend
          ready={ready}
          turnOpen={turnOpen}
          stopping={stopping}
          menuOpen={menuOpen}
          onSend={submit}
          onStop={onStop}
          onMenu={onMenuToggle}
        />
      )}
    </div>
  );
  return (
    <>
      {deadline !== null && (text.trim() !== "" || files.length > 0) && (
        <CacheLowBanner deadline={deadline} subject="draft" />
      )}
      {row}
    </>
  );
}

/**
 * Shared by the composer input and its measuring copy, so both wrap alike. The
 * frame around them draws the border, so a one-line input with it matches the buttons' height.
 */
const COMPOSER_INPUT =
  "voice-prose min-h-[3.375rem] min-w-0 resize-none bg-transparent px-3 text-sm";

/** How long stops of background tasks gather before one message wakes Claude. */
const WAKE_DELAY_MS = 5000;

/** The thin bar's background pill, or null with none running. */
function backgroundLabel(count: number): string | null {
  return count === 0 ? null : `${count} background`;
}

const THIN_BUTTON =
  "text-primary hover:bg-muted flex shrink-0 items-center gap-1 rounded-[5px] border px-1.5 leading-5 whitespace-nowrap";

/** The repository containing `path`, null while unknown or when none does. */
function useRepoRoot(path: string | null): string | null {
  const [root, setRoot] = useState<string | null>(null);
  useEffect(() => {
    setRoot(null);
    if (path === null) return;
    let alive = true;
    repoRoot(path)
      .then((found) => {
        if (alive) setRoot(found);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [path]);
  return root;
}

export function SessionScreen({ client, id }: { client: Client; id: string }) {
  const snapshot = useSnapshot(client);
  const transcript = snapshot.transcripts.get(id) ?? emptyTranscript;
  const history = snapshot.histories.get(id);
  const loadEarlier = useCallback(() => client.loadEarlier(id), [client, id]);
  const view = snapshot.sessions?.find((session) => session.id === id) ?? null;
  useSeeCooling(useMemo(() => (view === null ? [] : [view]), [view]));
  const repo = useRepoRoot(view?.cwd ?? null);
  const openDiff =
    view === null || repo === null
      ? null
      : () => {
          changeExplorer((state) => openSessionDiff(state, view.id, view.cwd, repo));
          navigate({ kind: "files" });
        };
  const serverError = snapshot.sessionErrors.get(id) ?? null;
  const [pending, setPending] = useState<ScopedSend[]>([]);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);
  const nextPending = useRef(0);

  useEffect(() => {
    client.openSession(id);
    return () => client.closeSession(id);
  }, [client, id]);

  // The agent whose discussion is on screen; null for the main one.
  const [viewing, setViewing] = useState<string | null>(() => viewed.get(id) ?? null);
  const [agentsOpen, setAgentsOpen] = useState(false);
  const switchTo = useCallback(
    (agent: string | null) => {
      if (agent === null) viewed.delete(id);
      else viewed.set(id, agent);
      setViewing(agent);
      setAgentsOpen(false);
    },
    [id],
  );
  const entries: AgentEntry[] = agentEntries(view?.agents ?? [], transcript.agents);
  const agent =
    viewing === null ? null : (entries.find((entry) => entry.toolUseId === viewing) ?? null);
  const shown = viewing === null ? transcript : (transcript.scopes.get(viewing) ?? emptyTranscript);
  const draftSlot = draftKey(id, viewing);
  const agentGauge = useCacheGauge(agent?.cache ?? null, null);
  const agentCold = agent === null ? null : agentColdAt(agent, agentGauge);
  /** The agent and cold spell a rewarm was confirmed for; relays go straight until the next spell. */
  const [agentRewarmed, setAgentRewarmed] = useState<{ agent: string; coldAt: number } | null>(
    null,
  );
  const agentColdPanel =
    agentCold !== null && !(agentRewarmed?.agent === viewing && agentRewarmed.coldAt === agentCold);

  // A pending message leaves once the server's `user_sent` or `relay_sent` for one of its attempts is in.
  const sent = sentClientIds(transcript);
  const unsent = pending.filter((send) => !send.clientIds.some((cid) => sent.has(cid)));
  useEffect(() => {
    if (unsent.length !== pending.length) setPending(unsent);
  }, [unsent, pending.length]);
  const visiblePending = unsent.filter((send) => send.scope === viewing);

  const patch = (sendId: string, change: Partial<PendingSend>) =>
    setPending((list) => list.map((send) => (send.id === sendId ? { ...send, ...change } : send)));

  const gauge = useCacheGauge(view?.cache ?? null, view?.updated_at ?? null);
  const [chosen, setChosen] = useState<ColdChoiceMade | null>(() => coldChoices.get(id) ?? null);
  // When the server refused on a cache this client counted warm, e.g. with clocks apart.
  const [refusedAt, setRefusedAt] = useState<number | null>(null);
  const lastRequestAt = view?.cache?.last_request_at ?? null;
  useEffect(() => {
    setRefusedAt(null);
  }, [lastRequestAt]);
  const settingsChangedAt = view?.settings_changed_at ?? null;
  const settingsAt = settingsChangedAt === null ? null : parseTime(settingsChangedAt);
  const spell = coldSpell(gauge, refusedAt, settingsAt);
  const mainDeadline = viewing === null ? cacheDeadline(gauge) : null;
  const rewarm = rewarmField(spell, chosen);

  const noteRefusal = (err: unknown) => {
    if (err instanceof CacheColdError) setRefusedAt(Date.now());
  };

  /** A send to the main agent, or a relay to the agent of `scope`. */
  const deliver = (
    sendId: string,
    scope: string | null,
    text: string,
    clientId: string,
    fields: SendFields,
  ) => {
    const target = scope === null ? null : entries.find((entry) => entry.toolUseId === scope);
    let command: Promise<void>;
    if (scope === null) {
      command = client.command({ type: "send", session: id, text, ...fields, client_id: clientId });
    } else if (target?.live !== true || target.taskId === null) {
      command = Promise.reject(
        new Error("This agent is not in the running Claude process: nothing can reach it"),
      );
    } else {
      command = client.command({
        type: "relay",
        session: id,
        task_id: target.taskId,
        text,
        ...fields,
        client_id: clientId,
      });
    }
    command.then(
      () => patch(sendId, { status: "sent" }),
      (err: unknown) => {
        noteRefusal(err);
        patch(sendId, { status: "failed", error: errorMessage(err) });
      },
    );
  };

  const submitAnswer: SubmitAnswer = (requestId, answer) =>
    client
      .command({
        type: "answer",
        session: id,
        request_id: requestId,
        answer,
        ...rewarm,
        client_id: client.newClientId(),
      })
      .catch((err: unknown) => {
        noteRefusal(err);
        throw err;
      });

  /** Sends to the discussion `scope` (null: the main one); `send` sends to the one on screen. */
  const sendTo = (scope: string | null, text: string, fields: SendFields) => {
    nextPending.current += 1;
    const clientId = client.newClientId();
    const entry: ScopedSend = {
      id: String(nextPending.current),
      text,
      attachments: fields.attachments ?? [],
      clientIds: [clientId],
      status: "sending",
      error: null,
      scope,
    };
    setPending((list) => [...list, entry]);
    deliver(entry.id, scope, text, clientId, fields);
  };

  /** In an agent's view, a relay. */
  const send = (text: string, fields: SendFields = rewarm) => sendTo(viewing, text, fields);

  const retry = (entry: PendingSend) => {
    const clientId = client.newClientId();
    const scope = pending.find((send) => send.id === entry.id)?.scope ?? null;
    patch(entry.id, { status: "sending", error: null, clientIds: [...entry.clientIds, clientId] });
    deliver(entry.id, scope, entry.text, clientId, {
      ...rewarm,
      ...attachmentsField(entry.attachments),
    });
  };

  const dismiss = (entry: PendingSend) =>
    setPending((list) => list.filter((item) => item.id !== entry.id));

  const stop = () => {
    setStopping(true);
    setStopError(null);
    client
      .command({ type: "interrupt", session: id, client_id: client.newClientId() })
      .catch((err: unknown) => setStopError(errorMessage(err)))
      .finally(() => setStopping(false));
  };

  // A failed stop is reported until its turn ends.
  const turnOpen = transcript.turnOpen;
  useEffect(() => {
    if (!turnOpen) setStopError(null);
  }, [turnOpen]);

  const busy = view?.busy ?? false;
  // Asks die with their process; a session known to have none shows no card.
  const asks = view?.live === false ? [] : transcript.asks;
  const ask = asks[0];
  const approvedPlan = transcript.approvedPlan;

  const [choosing, setChoosing] = useState(false);
  const [coldError, setColdError] = useState<string | null>(null);
  const archived = view !== null && view.archived_at !== null;
  const [flow, setFlow] = useState<SettingsFlow>(closedFlow);
  const [panel, setPanel] = useState<PanelState>(closedPanel);
  const coldPanel =
    archived ||
    flow.kind !== "closed" ||
    (view !== null &&
      coldPanelShown({ gauge, refusedAt, settingsAt, busy, asks: asks.length, chosen }));

  // The discussion's own catalog once its Claude started, else the one all discussions share.
  const shared = snapshot.catalog;
  const models = transcript.catalog?.models ?? shared?.models ?? [];
  const commands = transcript.catalog?.commands ?? shared?.commands ?? [];
  const terminalCommands =
    transcript.terminalCommands.length > 0
      ? transcript.terminalCommands
      : (shared?.terminal_commands ?? []);
  const current: Settings = { model: view?.model ?? null, effort: view?.effort ?? null };
  const picks = flow.kind === "closed" ? current : flow.picks;
  const fallbackModel = shared?.defaults?.model ?? null;
  const fallbackEffort = defaultEffort(models, picks.model, shared?.defaults?.effort ?? null);

  // A settings choice ends once the session list shows it; a rewarm then answers its spell.
  const outcome = appliedOutcome(flow, { ...current, settingsChangedAt: settingsAt });
  const applied = outcome.done;
  const rewarmSpell = outcome.done ? outcome.rewarmSpell : null;
  useEffect(() => {
    if (!applied) return;
    if (rewarmSpell !== null && spell !== null) {
      const made: ColdChoiceMade = { spell, choice: "rewarm" };
      coldChoices.set(id, made);
      setChosen(made);
    }
    setFlow(closedFlow);
  }, [applied, rewarmSpell, spell, id]);

  const [commandError, setCommandError] = useState<string | null>(null);
  const [menu, setMenu] = useState<MenuState>(closedMenu);
  /** Opens the menu on `entry`, or closes it when already open there. */
  const toggleMenu = (entry: MenuEntry | null) =>
    setMenu((current) =>
      current.kind === "open" && current.entry === entry ? closedMenu : { kind: "open", entry },
    );

  // The composer reads its draft when mounted: a change made from outside remounts it.
  const [composerEpoch, setComposerEpoch] = useState(0);
  const focusComposer = useRef(false);
  const refillComposer = (text: string, paths: readonly string[], focus: boolean) => {
    fillDraft(draftSlot, text, paths);
    focusComposer.current = focus;
    setComposerEpoch((epoch) => epoch + 1);
  };
  /** The composer's message, emptied; null when blank or still uploading. */
  const takeDraft = (): { text: string; attachments: string[] } | null => {
    const text = drafts.get(draftSlot) ?? "";
    const files = draftFiles.get(draftSlot) ?? [];
    const attachments = readyPaths(draftSlot);
    if (files.some((file) => file.status === "uploading")) return null;
    if (text.trim() === "" && attachments.length === 0) return null;
    refillComposer("", [], false);
    return { text, attachments };
  };
  /** A message the composer lost to a refused command goes back into it when empty. */
  const giveBack = (text: string, attachments: readonly string[]) => {
    if ((drafts.get(draftSlot) ?? "") !== "" || draftFiles.has(draftSlot)) return;
    refillComposer(text, attachments, false);
  };

  /** The held message open in the composer, and the draft it pushed aside. */
  const [editing, setEditing] = useState<{
    held: HeldMessage;
    stash: { text: string; attachments: string[] };
  } | null>(null);
  const endEdit = (stash: { text: string; attachments: string[] }) => {
    setEditing(null);
    refillComposer(stash.text, stash.attachments, false);
  };

  const hold = (text: string, attachments: string[], mode: HeldMode) => {
    setCommandError(null);
    client
      .command({
        type: "hold",
        session: id,
        text,
        ...attachmentsField(attachments),
        mode,
        client_id: client.newClientId(),
      })
      .catch((err: unknown) => {
        noteRefusal(err);
        setCommandError(`Not queued: ${errorMessage(err)}`);
        giveBack(text, attachments);
      });
  };

  const heldCommand = (command: Promise<void>, what: string) => {
    setCommandError(null);
    command.catch((err: unknown) => {
      noteRefusal(err);
      setCommandError(`${what}: ${errorMessage(err)}`);
    });
  };
  const heldActions: HeldActions = {
    editing: editing?.held.id ?? null,
    onSendNow: (held) =>
      heldCommand(
        client.command({
          type: "send_held",
          session: id,
          held_id: held.id,
          ...rewarm,
          client_id: client.newClientId(),
        }),
        "Not sent",
      ),
    onEdit: (held) => {
      const stash = editing?.stash ?? {
        text: drafts.get(draftSlot) ?? "",
        attachments: readyPaths(draftSlot),
      };
      setEditing({ held, stash });
      refillComposer(held.text, held.attachments, true);
    },
    onMode: (held, mode) =>
      heldCommand(
        client.command({
          type: "edit_held",
          session: id,
          held_id: held.id,
          mode,
          client_id: client.newClientId(),
        }),
        "Not changed",
      ),
    onDrop: (held) =>
      heldCommand(
        client.command({
          type: "drop_held",
          session: id,
          held_id: held.id,
          client_id: client.newClientId(),
        }),
        "Not deleted",
      ),
  };

  /** Saves the composer into the held message being edited. */
  const saveEdit = (text: string, attachments: string[]) => {
    if (editing === null) return;
    const { held, stash } = editing;
    setCommandError(null);
    client
      .command({
        type: "edit_held",
        session: id,
        held_id: held.id,
        text,
        attachments,
        client_id: client.newClientId(),
      })
      .then(
        () => endEdit(stash),
        (err: unknown) => {
          // The edit stays in the composer; the pushed-aside draft is dropped only if never restored.
          setEditing(null);
          setCommandError(
            err instanceof HeldGoneError
              ? "That message was already sent: your edit stays in the composer"
              : `Not saved: ${errorMessage(err)}`,
          );
          refillComposer(text, attachments, true);
        },
      );
  };

  /** Send from the composer: during a turn of the main agent, queued for its end. */
  const composerSend = (text: string, attachments: string[]) => {
    if (editing !== null) {
      saveEdit(text, attachments);
      return;
    }
    if (viewing === null && transcript.turnOpen) {
      hold(text, attachments, "turn_end");
      return;
    }
    send(text, { ...rewarm, ...attachmentsField(attachments) });
  };

  /** Starts the change flow; the menu opens on `entry`, which lists its choices. */
  const startSettings = (entry: "model" | "effort") => {
    setPanel(closedPanel);
    setMenu({ kind: "open", entry });
    setColdError(null);
    if (transcript.turnOpen) {
      client
        .command({ type: "interrupt", session: id, client_id: client.newClientId() })
        .catch((err: unknown) => setStopError(errorMessage(err)));
    }
    setFlow({ kind: "choosing", picks: current });
  };

  /** The change flow's panel on the current settings: its first choice resumes untouched. */
  const startRestart = () => {
    setPanel(closedPanel);
    setMenu(closedMenu);
    setColdError(null);
    setFlow({ kind: "choosing", picks: current, restart: true });
  };

  const choosePicks = (next: Settings) => {
    if (flow.kind === "choosing") setFlow({ ...flow, picks: next });
  };

  const chooseSettings = (choice: ColdChoice) => {
    if (flow.kind !== "choosing") return;
    const chosenPicks = flow.picks;
    const plan = settingsPlan(chosenPicks, current, choice, drafts.get(id) ?? "", readyPaths(id));
    setColdError(null);
    if (plan.then.kind === "close") {
      setFlow(closedFlow);
      return;
    }
    const then = plan.then;
    setChoosing(true);
    setFlow({ kind: "applying", picks: chosenPicks, choice });
    const stored =
      plan.patch === null
        ? Promise.resolve()
        : client.command({
            type: "set_settings",
            session: id,
            ...plan.patch,
            client_id: client.newClientId(),
          });
    stored
      .then(async () => {
        if (then.kind === "rewarm") {
          if (then.text !== null) {
            drafts.delete(id);
            draftFiles.delete(id);
            sendTo(null, then.text, { rewarm: true, ...attachmentsField(then.attachments ?? []) });
          }
          return;
        }
        await client.command({
          type: "restart",
          session: id,
          mode: then.mode,
          ...(then.text === null ? {} : { text: then.text }),
          ...attachmentsField(then.attachments ?? []),
          client_id: client.newClientId(),
        });
        drafts.delete(id);
        draftFiles.delete(id);
      })
      .catch((err: unknown) => {
        setColdError(errorMessage(err));
        setFlow({ kind: "choosing", picks: chosenPicks });
      })
      .finally(() => setChoosing(false));
  };

  const runOwn = (command: OwnCommand) => {
    switch (command) {
      case "clear":
        setCommandError(null);
        client
          .command({
            type: "restart",
            session: id,
            mode: "clear",
            client_id: client.newClientId(),
          })
          .catch((err: unknown) => setCommandError(errorMessage(err)));
        return;
      case "model":
      case "effort":
        startSettings(command);
        return;
    }
  };

  const runEffect = (effect: PanelEffect) => {
    switch (effect.kind) {
      case "none":
        return;
      case "send":
        sendTo(null, effect.text, rewarm);
        return;
      case "own":
        runOwn(effect.command);
        return;
    }
  };

  const panelEvent = (event: PanelEvent) => {
    const next = panelStep(panel, event);
    setPanel(next.state);
    runEffect(next.effect);
  };

  const allCards = commandCards(commands, terminalCommands);
  const claudeCards = allCards.filter((card) => card.own === null);
  const argumentCard: CommandCard | null =
    panel.kind === "argument"
      ? (allCards.find((card) => card.own === null && card.name === commandQuery(panel.text)) ?? {
          name: commandQuery(panel.text),
          title: null,
          description: "",
          argumentHint: "",
          own: null,
        })
      : null;
  const shownCards = panel.kind === "browsing" ? filterCards(allCards, panel.filter) : [];
  const controlsEnabled = view !== null && !archived;
  const choosingSettings = flow.kind !== "closed";

  const mode =
    view?.live === true
      ? transcript.permissionMode
      : (view?.permission_mode ?? transcript.permissionMode);
  const modeControl = usePermissionMode(client, id, mode, controlsEnabled);
  const modelLabel = modelTabLabel(
    models,
    view?.applied_model ?? null,
    picks.model,
    choosingSettings,
    fallbackModel,
  );
  const effort = shownEffort(
    view?.applied_effort ?? null,
    picks.effort,
    choosingSettings,
    fallbackEffort,
  );

  // Revoking drops the granted folders with the Claude process; the next send
  // resumes the same Claude session with `--resume`, keeping its context.
  const [revoking, setRevoking] = useState(false);
  const [revokeError, setRevokeError] = useState<string | null>(null);
  const revoke = () => {
    setRevoking(true);
    setRevokeError(null);
    client
      .command({ type: "kill", session: id, reason: "revoke", client_id: client.newClientId() })
      .then(
        () => setMenu(closedMenu),
        (err: unknown) => setRevokeError(errorMessage(err)),
      )
      .finally(() => setRevoking(false));
  };

  const liveAgents = view?.agents ?? [];
  const [stoppingAgents, setStoppingAgents] = useState(false);
  const [stopAgentsError, setStopAgentsError] = useState<string | null>(null);
  const stopAllAgents = () => {
    setStoppingAgents(true);
    setStopAgentsError(null);
    client
      .command({ type: "stop_agents", session: id, client_id: client.newClientId() })
      .then(
        () => setMenu(closedMenu),
        (err: unknown) => setStopAgentsError(errorMessage(err)),
      )
      .finally(() => setStoppingAgents(false));
  };
  const [bottomRequest, setBottomRequest] = useState(0);
  const stopAgent = (taskId: string) =>
    client.command({
      type: "stop_task",
      session: id,
      task_id: taskId,
      client_id: client.newClientId(),
    });
  // The call fails and the turn goes on, so nothing to wake.
  const stopTool = useCallback(
    async (taskId: string) => {
      await client.command({
        type: "stop_task",
        session: id,
        task_id: taskId,
        client_id: client.newClientId(),
      });
    },
    [client, id],
  );

  // A stopped background task does not wake Claude. Stops made while it waits for
  // them are gathered for a while, then one message wakes it, on a warm cache only.
  const wakeUp = useRef<{ names: string[]; timer: ReturnType<typeof setTimeout> | null }>({
    names: [],
    timer: null,
  });
  const latest = useRef({ warm: false, turnOpen: false, sendTo });
  useEffect(() => {
    latest.current = { warm: gauge.kind === "warm" && refusedAt === null, turnOpen, sendTo };
  });
  useEffect(
    () => () => {
      if (wakeUp.current.timer !== null) clearTimeout(wakeUp.current.timer);
    },
    [],
  );
  const stopBackground = async (taskId: string) => {
    const waiting = !latest.current.turnOpen;
    const task = view?.background.find((candidate) => candidate.task_id === taskId);
    await stopAgent(taskId);
    if (!waiting) return;
    const pending = wakeUp.current;
    pending.names.push(task?.description ?? taskId);
    if (pending.timer !== null) clearTimeout(pending.timer);
    pending.timer = setTimeout(() => {
      const names = pending.names;
      pending.names = [];
      pending.timer = null;
      const now = latest.current;
      if (!now.warm || now.turnOpen) return;
      now.sendTo(null, wakeText(names), {});
    }, WAKE_DELAY_MS);
  };
  const openAgents = () => {
    setMenu(closedMenu);
    setPanel(closedPanel);
    setAgentsOpen(true);
  };
  const askGoTo = ask === undefined ? null : goToAgent(ask, viewing, entries);

  // Escape closes the menu, else the commands panel, else the agents panel, else stops a running turn.
  const escape = useRef(() => {});
  useLayoutEffect(() => {
    escape.current = () => {
      if (menu.kind === "open") setMenu(closedMenu);
      else if (panel.kind !== "closed") setPanel(closedPanel);
      else if (agentsOpen) setAgentsOpen(false);
      else if (transcript.turnOpen && !stopping) stop();
    };
  });
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
      escape.current();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const choose = (choice: ColdChoice) => {
    if (spell === null && !archived) return;
    // A choice made from the archived view also answers the cold spell, if any.
    const settle = () => {
      if (spell === null) return;
      const made = { spell, choice };
      coldChoices.set(id, made);
      setChosen(made);
    };
    // A draft typed for an agent goes with a rewarm, as a relay; a restart ends
    // the agents, so the new session starts without it.
    const draft = drafts.get(draftSlot) ?? "";
    const files = readyPaths(draftSlot);
    const action = archived
      ? archivedAction(choice, draft, files)
      : coldAction(choice, draft, files);
    const restartText = action.kind === "restart" && viewing === null ? action.text : null;
    const restartFiles =
      action.kind === "restart" && viewing === null ? (action.attachments ?? []) : [];
    setColdError(null);
    switch (action.kind) {
      case "unblock":
        settle();
        return;
      case "restore":
        setChoosing(true);
        client
          .command({ type: "restore", session: id, client_id: client.newClientId() })
          .then(settle, (err: unknown) => setColdError(errorMessage(err)))
          .finally(() => setChoosing(false));
        return;
      case "send":
        drafts.delete(draftSlot);
        draftFiles.delete(draftSlot);
        settle();
        send(action.text, { rewarm: true, ...attachmentsField(action.attachments ?? []) });
        return;
      case "restart":
        setChoosing(true);
        client
          .command({
            type: "restart",
            session: id,
            mode: action.mode,
            ...(restartText === null ? {} : { text: restartText }),
            ...attachmentsField(restartFiles),
            client_id: client.newClientId(),
          })
          .then(
            () => {
              if (restartText !== null) {
                drafts.delete(draftSlot);
                draftFiles.delete(draftSlot);
              }
              settle();
            },
            (err: unknown) => setColdError(errorMessage(err)),
          )
          .finally(() => setChoosing(false));
        return;
    }
  };

  const viewId = view?.id ?? null;
  const cwd = view?.cwd ?? null;
  const fileLinks = useMemo<FileLinks>(
    () => ({
      cwd,
      open: (target, line) => {
        changeExplorer((state) => {
          if (viewId === null || cwd === null) {
            return target.kind === "file"
              ? openFileInLastFree(state, target.path, line)
              : openFolderInLastFree(state, target.path);
          }
          return target.kind === "file"
            ? openSessionFile(state, viewId, cwd, target.path, line)
            : openSessionFolderAt(state, viewId, cwd, target.path);
        });
        navigateBackLeaves();
      },
    }),
    [viewId, cwd],
  );

  return (
    <FileLinksContext.Provider value={fileLinks}>
      <Header
        left={<HomeButton sessions={snapshot.sessions} onScreen={id} />}
        status={snapshot.status}
        busy={busy}
        right={<SearchButton discussion={id} />}
        usage={snapshot.usage}
        column={
          <SessionColumn
            session={view}
            {...(viewing === null
              ? {}
              : {
                  agent: { cache: agent?.cache ?? null, promptTokens: agent?.promptTokens ?? 0 },
                })}
          />
        }
        info={
          <>
            {viewing !== null && (
              <button
                type="button"
                onClick={() => switchTo(null)}
                className="mr-1 rounded-[5px] px-1 text-primary hover:bg-muted"
              >
                &lsaquo; main
              </button>
            )}
            <span>{view === null ? "..." : sessionTitle(view)}</span>
            {viewing !== null && (
              <span>
                {" "}
                &rsaquo; {agent?.name ?? transcript.agents.get(viewing)?.name ?? "agent"}
              </span>
            )}
            {view !== null && viewing === null && (
              <span className="text-muted-foreground">
                {" "}
                {busy ? "working" : archived ? "archived" : stateLabel(view.state)}
              </span>
            )}
          </>
        }
        pills={
          <MenuPills
            mode={mode}
            modelLabel={modelPillLabel(modelLabel, effort)}
            enabled={controlsEnabled}
            onOpen={toggleMenu}
            agents={workingLabel(liveAgents)}
            onAgents={() => (agentsOpen ? setAgentsOpen(false) : openAgents())}
            background={backgroundLabel(view?.background.length ?? 0)}
            onBackground={() => {
              // The rows live at the end of the main discussion.
              if (viewing !== null) switchTo(null);
              setBottomRequest((count) => count + 1);
            }}
          />
        }
        end={
          view !== null && (
            <>
              {approvedPlan !== null && (
                <button
                  type="button"
                  onClick={() => {
                    changeExplorer((state) =>
                      openSessionFile(state, view.id, view.cwd, approvedPlan),
                    );
                    navigateBackLeaves();
                  }}
                  title={approvedPlan}
                  className={THIN_BUTTON}
                >
                  <FileIcon className="size-3.5 text-mode-plan" />
                  <span className="max-sm:hidden">Plan</span>
                </button>
              )}
              {repo !== null && openDiff !== null && (
                <button
                  type="button"
                  onClick={openDiff}
                  title={`Pending changes in ${folderName(repo)}`}
                  className={THIN_BUTTON}
                >
                  <DiffIcon className="size-3.5 text-git-modified" />
                  <span className="max-sm:hidden">Diff</span>
                </button>
              )}
              <button
                type="button"
                onClick={() => {
                  changeExplorer((state) => openSessionFolder(state, view.id, view.cwd));
                  navigate({ kind: "files" });
                }}
                title={view.cwd}
                className={THIN_BUTTON}
              >
                <FolderIcon className="size-3.5" />
                <span className="max-sm:hidden">Project folder</span>
              </button>
            </>
          )
        }
      />

      {serverError !== null && (
        <p className="border-b px-3 py-2 text-sm text-destructive">{serverError}</p>
      )}

      <TranscriptView
        key={viewing ?? "main"}
        loading={!snapshot.transcripts.has(id)}
        firstSeq={transcript.firstSeq}
        beginning={history?.beginning ?? "loaded"}
        request={history?.request ?? "idle"}
        onNearTop={loadEarlier}
        items={shown.items}
        pending={visiblePending}
        onRetry={retry}
        onDismiss={dismiss}
        held={viewing === null ? (view?.held ?? []) : []}
        heldActions={heldActions}
        awaiting={new Set(asks.map((pendingAsk) => pendingAsk.toolUseId))}
        turnOpen={shown.turnOpen}
        onOpen={switchTo}
        onOpenAttachment={(path) => fileLinks.open({ kind: "file", path }, null)}
        onOpenDiff={openDiff}
        glowTurnEnd={viewing === null}
        background={viewing === null ? (view?.background ?? []) : []}
        onStopTask={stopBackground}
        toolTasks={transcript.toolTasks}
        onStopTool={stopTool}
        bottomRequest={bottomRequest}
        end={viewing === null && !transcript.turnOpen && <RestartBanner session={view} />}
        panel={
          panel.kind !== "closed" ? (
            <CommandsPanel
              cards={shownCards}
              argument={argumentCard}
              onUse={(card) => panelEvent({ type: "use", card })}
              onClose={() => panelEvent({ type: "close" })}
            />
          ) : menu.kind === "open" ? (
            <SessionMenu
              entry={menu.entry}
              enabled={controlsEnabled}
              picking={choosingSettings}
              name={
                view === null
                  ? null
                  : {
                      current: view.name,
                      folder: folderName(view.cwd),
                      onSave: (name) =>
                        client.command({
                          type: "rename",
                          session: id,
                          name,
                          client_id: client.newClientId(),
                        }),
                    }
              }
              mode={mode}
              modeControl={modeControl}
              model={{
                label: modelLabel,
                stopLabel: "Stop here and change model",
                options: models.map((model) => ({
                  value: model.value,
                  label: modelOptionLabel(models, model),
                })),
                selected: picks.model ?? DEFAULT_MODEL,
                onStop: () => startSettings("model"),
                onPick: (value) => choosePicks(pickModel(picks, value, models)),
              }}
              effort={{
                label: effortTabLabel(
                  view?.applied_effort ?? null,
                  picks.effort,
                  choosingSettings,
                  fallbackEffort,
                ),
                stopLabel: "Stop here and change effort",
                options: effortLevels(models, picks.model).map((level) => ({
                  value: level,
                  label: level,
                })),
                selected: picks.effort,
                onStop: () => startSettings("effort"),
                onPick: (value) => choosePicks(pickEffort(picks, value)),
              }}
              commandsEnabled={controlsEnabled && !choosingSettings}
              revoke={{
                // Revoking ends the process, so a running turn would be cut short.
                enabled: controlsEnabled && view?.live === true && !turnOpen,
                busy: revoking,
                error: revokeError,
                onRevoke: revoke,
              }}
              restartEnabled={controlsEnabled && !choosingSettings}
              onRestart={startRestart}
              agents={{
                summary: workingLabel(liveAgents) ?? `${entries.length} known`,
                onOpen: openAgents,
                stopAll: {
                  enabled: liveAgents.some((running) => running.state === "running"),
                  busy: stoppingAgents,
                  error: stopAgentsError,
                  onStop: stopAllAgents,
                },
              }}
              draft={
                viewing !== null ||
                editing !== null ||
                ((drafts.get(draftSlot) ?? "").trim() === "" && readyPaths(draftSlot).length === 0)
                  ? null
                  : {
                      onHold: () => {
                        setMenu(closedMenu);
                        const draft = takeDraft();
                        if (draft !== null) hold(draft.text, draft.attachments, "held");
                      },
                      onSendNow: transcript.turnOpen
                        ? () => {
                            setMenu(closedMenu);
                            const draft = takeDraft();
                            if (draft !== null) {
                              send(draft.text, {
                                ...rewarm,
                                ...attachmentsField(draft.attachments),
                              });
                            }
                          }
                        : null,
                    }
              }
              onEntry={(entry) => setMenu({ kind: "open", entry })}
              onCommands={() => {
                setMenu(closedMenu);
                panelEvent({ type: "open_tab" });
              }}
              onClose={() => setMenu(closedMenu)}
            />
          ) : agentsOpen ? (
            <AgentsPanel
              entries={recentAgents(entries, viewing, Date.now())}
              viewing={viewing}
              onSwitch={switchTo}
              onStop={stopAgent}
              onClose={() => setAgentsOpen(false)}
            />
          ) : undefined
        }
      />

      {panel.kind !== "closed" ? (
        <footer className="border-t px-3 pt-1 pb-[max(0.25rem,env(safe-area-inset-bottom))]">
          <CommandField
            state={panel}
            draftKept={(drafts.get(draftSlot) ?? "").trim() !== ""}
            onFilter={(text) => panelEvent({ type: "filter", text })}
            onArgument={(text) => panelEvent({ type: "argument", text })}
            onSend={() => panelEvent({ type: "send" })}
            onClose={() => panelEvent({ type: "close" })}
          />
        </footer>
      ) : coldPanel ? (
        <ColdPanel
          key={flow.kind === "closed" ? "cold" : "settings"}
          archived={archived}
          context={view?.context ?? null}
          draft={drafts.get(draftSlot) ?? ""}
          askWaiting={ask !== undefined}
          busy={choosing || flow.kind === "applying"}
          error={coldError}
          settings={
            flow.kind === "closed"
              ? null
              : {
                  changed: firstButton(picks, current) === "rewarm",
                  restart: flow.kind === "choosing" && flow.restart === true,
                  onCancel: () => {
                    setColdError(null);
                    setFlow(closedFlow);
                  },
                }
          }
          onChoose={flow.kind === "closed" ? choose : chooseSettings}
        />
      ) : (
        <>
          {ask !== undefined && (
            <AskCard
              key={ask.requestId}
              submit={submitAnswer}
              ask={ask}
              queued={asks.length - 1}
              cwd={view?.cwd ?? null}
              context={askContext(transcript, ask.toolUseId)}
              deadline={mainDeadline}
              sessionModel={modelLabel}
              goTo={
                askGoTo === null
                  ? null
                  : { name: askGoTo.name, onGo: () => switchTo(askGoTo.toolUseId) }
              }
            />
          )}

          {agentColdPanel && agent !== null && agentCold !== null ? (
            <AgentColdPanel
              name={agent.name}
              promptTokens={agent.promptTokens}
              draft={drafts.get(draftSlot) ?? ""}
              onRewarm={() => setAgentRewarmed({ agent: agent.toolUseId, coldAt: agentCold })}
              onBack={() => switchTo(null)}
            />
          ) : (
            <footer className="border-t px-3 pt-1 pb-[max(0.25rem,env(safe-area-inset-bottom))]">
              {commandError !== null && (
                <p className="mx-auto max-w-5xl text-xs voice-ui text-destructive">
                  {commandError}
                </p>
              )}
              {stopError !== null && (
                <p className="mx-auto max-w-5xl text-xs voice-ui text-destructive">
                  Stop failed: {stopError}
                </p>
              )}
              {editing !== null && viewing === null && (
                <div className="mx-auto flex max-w-5xl items-center justify-between gap-2 pb-1 text-xs voice-ui">
                  <span className="text-muted-foreground">
                    {(view?.held ?? []).some((held) => held.id === editing.held.id)
                      ? "Editing queued message"
                      : "This queued message was sent meanwhile"}
                  </span>
                  <button
                    type="button"
                    onClick={() => endEdit(editing.stash)}
                    className="text-primary underline"
                  >
                    Cancel
                  </button>
                </div>
              )}
              <Composer
                key={`${draftSlot}:${composerEpoch}`}
                session={draftSlot}
                uploadTo={id}
                takeFocus={() => {
                  const focus = focusComposer.current;
                  focusComposer.current = false;
                  return client.takeCreated(id) || focus;
                }}
                deadline={mainDeadline}
                placeholder={
                  viewing === null
                    ? "Message Claude"
                    : `Message ${agent?.name ?? "the agent"}, relayed by the main agent`
                }
                turnOpen={transcript.turnOpen}
                stopping={stopping}
                menuOpen={menu.kind === "open"}
                commands={claudeCards}
                recentCommands={snapshot.recentCommands}
                onSuggest={() => void client.refreshRecentCommands()}
                onSend={composerSend}
                onStop={stop}
                onMenu={() => setMenu({ kind: "open", entry: null })}
                onMenuToggle={() =>
                  setMenu((current) =>
                    current.kind === "open" ? closedMenu : { kind: "open", entry: null },
                  )
                }
              />
            </footer>
          )}
        </>
      )}
    </FileLinksContext.Provider>
  );
}
