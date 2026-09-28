import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from "react";

import type { Client } from "@/client";
import {
  CacheColdError,
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
  slashOpens,
} from "@/commands";
import { AskCard, type SubmitAnswer } from "@/components/AskCard";
import { ColdPanel, useCacheGauge } from "@/components/ColdPanel";
import { CommandField, CommandsPanel } from "@/components/CommandsPanel";
import { usePermissionMode } from "@/components/PermissionMode";
import { DesktopSend, TouchSend } from "@/components/SendControls";
import { BusyDot, ConnectionDot } from "@/components/SessionList";
import {
  type MenuEntry,
  type MenuState,
  MenuPills,
  SessionMenu,
  closedMenu,
} from "@/components/SessionMenu";
import { Header, SessionColumn } from "@/components/TopBar";
import { type PendingSend, TranscriptView } from "@/components/TranscriptView";
import { useSnapshot } from "@/components/useSnapshot";
import { folderName, stateLabel } from "@/format";
import { backToList } from "@/route";
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
import { type Item, emptyTranscript, parseTime } from "@/transcript";

/** Unsent composer text per session, kept while the app stays open. */
const drafts = new Map<string, string>();
/** Per session, the last cold-cache choice and the spell it was made for. */
const coldChoices = new Map<string, ColdChoiceMade>();

function sentClientIds(items: readonly Item[]): Set<string> {
  const ids = new Set<string>();
  for (const item of items) {
    if (item.kind === "user" && item.clientId !== null) ids.add(item.clientId);
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
  turnOpen,
  stopping,
  menuOpen,
  onSend,
  onSlash,
  onStop,
  onMenu,
  onMenuToggle,
}: {
  session: string;
  turnOpen: boolean;
  /** A stop was sent and awaits the server. */
  stopping: boolean;
  menuOpen: boolean;
  onSend: (text: string) => void;
  /** A `/` became the first character: `text` is the whole composer text. */
  onSlash: (text: string) => void;
  onStop: () => void;
  /** Opens the menu (swipe up on touch). */
  onMenu: () => void;
  /** Opens or closes the menu (the desktop button). */
  onMenuToggle: () => void;
}) {
  const [text, setText] = useState(() => drafts.get(session) ?? "");
  const area = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    const element = area.current;
    if (element === null) return;
    element.style.height = "auto";
    element.style.height = `${element.scrollHeight}px`;
  }, [text]);

  const change = (value: string) => {
    const opens = slashOpens(text, value);
    setText(value);
    if (value === "") drafts.delete(session);
    else drafts.set(session, value);
    if (opens) onSlash(value);
  };

  const submit = () => {
    if (text.trim() === "") return;
    onSend(text);
    change("");
    // On touch, dropping focus closes the virtual keyboard so the reply is visible.
    if (coarsePointer()) area.current?.blur();
  };

  // Desktop: Enter sends, Shift+Enter inserts a newline. Touch: Enter inserts a
  // newline and only the send button sends. An IME composition keeps its own Enter.
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
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
  const ready = text.trim() !== "";
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

  // Send spans the bar's height at any number of lines: the row stretches its items.
  return (
    <div className="mx-auto flex max-w-5xl items-stretch gap-2">
      <textarea
        ref={area}
        rows={1}
        value={text}
        onChange={(event) => change(event.target.value)}
        onKeyDown={onKeyDown}
        enterKeyHint={coarse ? "enter" : "send"}
        placeholder="Message Claude"
        className="bg-card focus:ring-ring voice-prose max-h-[35dvh] min-h-14 min-w-0 flex-1 resize-none rounded-[5px] border px-3 py-4 text-sm outline-none focus:ring-2"
      />
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
}

export function SessionScreen({
  client,
  id,
  label,
}: {
  client: Client;
  id: string;
  label: string | null;
}) {
  const snapshot = useSnapshot(client);
  const transcript = snapshot.transcripts.get(id) ?? emptyTranscript;
  const view = snapshot.sessions?.find((session) => session.id === id) ?? null;
  const serverError = snapshot.sessionErrors.get(id) ?? null;
  const [pending, setPending] = useState<PendingSend[]>([]);
  const [stopping, setStopping] = useState(false);
  const [stopError, setStopError] = useState<string | null>(null);
  const nextPending = useRef(0);

  useEffect(() => {
    client.openSession(id);
    return () => client.closeSession(id);
  }, [client, id]);

  // A pending message leaves once the server's `user_sent` for one of its attempts is in.
  const sent = sentClientIds(transcript.items);
  const visiblePending = pending.filter((send) => !send.clientIds.some((cid) => sent.has(cid)));
  useEffect(() => {
    if (visiblePending.length !== pending.length) setPending(visiblePending);
  }, [visiblePending, pending.length]);

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
  const rewarm = rewarmField(spell, chosen);

  const noteRefusal = (err: unknown) => {
    if (err instanceof CacheColdError) setRefusedAt(Date.now());
  };

  const deliver = (
    sendId: string,
    text: string,
    clientId: string,
    fields: { rewarm?: true },
  ) => {
    client.command({ type: "send", session: id, text, ...fields, client_id: clientId }).then(
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

  const send = (text: string, fields: { rewarm?: true } = rewarm) => {
    nextPending.current += 1;
    const clientId = client.newClientId();
    const entry: PendingSend = {
      id: String(nextPending.current),
      text,
      clientIds: [clientId],
      status: "sending",
      error: null,
    };
    setPending((list) => [...list, entry]);
    deliver(entry.id, text, clientId, fields);
  };

  const retry = (entry: PendingSend) => {
    const clientId = client.newClientId();
    patch(entry.id, { status: "sending", error: null, clientIds: [...entry.clientIds, clientId] });
    deliver(entry.id, entry.text, clientId, rewarm);
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

  const choosePicks = (next: Settings) => {
    if (flow.kind === "choosing") setFlow({ kind: "choosing", picks: next });
  };

  const chooseSettings = (choice: ColdChoice) => {
    if (flow.kind !== "choosing") return;
    const chosenPicks = flow.picks;
    const plan = settingsPlan(chosenPicks, current, choice, drafts.get(id) ?? "");
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
            send(then.text, { rewarm: true });
          }
          return;
        }
        await client.command({
          type: "restart",
          session: id,
          mode: then.mode,
          ...(then.text === null ? {} : { text: then.text }),
          client_id: client.newClientId(),
        });
        drafts.delete(id);
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
      case "draft":
        if (effect.text === "") drafts.delete(id);
        else drafts.set(id, effect.text);
        return;
      case "send":
        if (effect.consumeDraft) drafts.delete(id);
        send(effect.text);
        return;
      case "own":
        if (effect.consumeDraft) drafts.delete(id);
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
  const argumentCard: CommandCard | null =
    panel.kind === "argument"
      ? (allCards.find(
          (card) => card.own === null && card.name === commandQuery(panel.text),
        ) ?? {
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
  const effort = shownEffort(view?.applied_effort ?? null, picks.effort, choosingSettings, fallbackEffort);

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

  // Escape closes the menu, else the commands panel, else stops a running turn.
  const escape = useRef(() => {});
  useLayoutEffect(() => {
    escape.current = () => {
      if (menu.kind === "open") setMenu(closedMenu);
      else if (panel.kind !== "closed") setPanel(closedPanel);
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
    const draft = drafts.get(id) ?? "";
    const action = archived ? archivedAction(choice, draft) : coldAction(choice, draft);
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
        drafts.delete(id);
        settle();
        send(action.text, { rewarm: true });
        return;
      case "restart":
        setChoosing(true);
        client
          .command({
            type: "restart",
            session: id,
            mode: action.mode,
            ...(action.text === null ? {} : { text: action.text }),
            client_id: client.newClientId(),
          })
          .then(
            () => {
              drafts.delete(id);
              settle();
            },
            (err: unknown) => setColdError(errorMessage(err)),
          )
          .finally(() => setChoosing(false));
        return;
    }
  };

  return (
    <>
      <Header
        left={
          <button
            type="button"
            onClick={backToList}
            className="text-primary voice-ui hover:bg-muted shrink-0 rounded-[5px] px-2 py-1 text-sm"
          >
            &lsaquo; Sessions
          </button>
        }
        right={
          <>
            <ConnectionDot status={snapshot.status} />
            <BusyDot busy={busy} />
          </>
        }
        usage={snapshot.usage}
        column={<SessionColumn session={view} />}
        info={
          <>
            <span>{view === null ? "..." : folderName(view.cwd)}</span>
            {view !== null && (
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
          />
        }
        label={label}
      />

      {serverError !== null && (
        <p className="text-destructive border-b px-3 py-2 text-sm">{serverError}</p>
      )}

      <TranscriptView
        items={transcript.items}
        pending={visiblePending}
        onRetry={retry}
        onDismiss={dismiss}
        awaiting={new Set(asks.map((pendingAsk) => pendingAsk.toolUseId))}
        turnOpen={transcript.turnOpen}
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
              onEntry={(entry) => setMenu({ kind: "open", entry })}
              onCommands={() => {
                setMenu(closedMenu);
                panelEvent({ type: "open_tab" });
              }}
              onClose={() => setMenu(closedMenu)}
            />
          ) : undefined
        }
      />

      {panel.kind !== "closed" ? (
        <footer className="border-t px-3 pt-1 pb-[max(0.25rem,env(safe-area-inset-bottom))]">
          <CommandField
            state={panel}
            draftKept={panel.source === "tab" && (drafts.get(id) ?? "").trim() !== ""}
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
          draft={drafts.get(id) ?? ""}
          askWaiting={ask !== undefined}
          busy={choosing || flow.kind === "applying"}
          error={coldError}
          settings={
            flow.kind === "closed"
              ? null
              : {
                  changed: firstButton(picks, current) === "rewarm",
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
            />
          )}

          <footer className="border-t px-3 pt-1 pb-[max(0.25rem,env(safe-area-inset-bottom))]">
            {commandError !== null && (
              <p className="text-destructive voice-ui mx-auto max-w-5xl text-xs">{commandError}</p>
            )}
            {stopError !== null && (
              <p className="text-destructive voice-ui mx-auto max-w-5xl text-xs">
                Stop failed: {stopError}
              </p>
            )}
            <Composer
              session={id}
              turnOpen={transcript.turnOpen}
              stopping={stopping}
              menuOpen={menu.kind === "open"}
              onSend={send}
              onSlash={(text) => {
                setMenu(closedMenu);
                panelEvent({ type: "open_slash", text });
              }}
              onStop={stop}
              onMenu={() => setMenu({ kind: "open", entry: null })}
              onMenuToggle={() =>
                setMenu((current) =>
                  current.kind === "open" ? closedMenu : { kind: "open", entry: null },
                )
              }
            />
          </footer>
        </>
      )}
    </>
  );
}
