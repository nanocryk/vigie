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
import { PermissionMode } from "@/components/PermissionMode";
import { BusyDot, ConnectionDot } from "@/components/SessionList";
import { SettingTab } from "@/components/SettingTab";
import { Header, SessionColumn } from "@/components/TopBar";
import { type PendingSend, TranscriptView } from "@/components/TranscriptView";
import { useSnapshot } from "@/components/useSnapshot";
import { folderName, stateLabel } from "@/format";
import { backToList } from "@/route";
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
  modelTabLabel,
  pickEffort,
  pickModel,
  settingsPlan,
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

/** Send and the floating Stop above it share this size. */
const ACTION_SIZE = "voice-ui h-12 w-20 shrink-0 rounded-[5px] text-sm";

function Composer({
  session,
  onSend,
  onSlash,
}: {
  session: string;
  onSend: (text: string) => void;
  /** A `/` became the first character: `text` is the whole composer text. */
  onSlash: (text: string) => void;
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

  return (
    <div className="mx-auto flex max-w-5xl items-end gap-2">
      <textarea
        ref={area}
        rows={1}
        value={text}
        onChange={(event) => change(event.target.value)}
        onKeyDown={onKeyDown}
        enterKeyHint={coarsePointer() ? "enter" : "send"}
        placeholder="Message Claude"
        className="bg-card focus:ring-ring voice-prose max-h-[35dvh] min-h-12 flex-1 resize-none rounded-[5px] border px-3 py-3.5 text-sm outline-none focus:ring-2"
      />
      <button
        type="button"
        // Keeps the focus, and with it the OS keyboard, in the textarea.
        onMouseDown={(event) => event.preventDefault()}
        onClick={submit}
        disabled={text.trim() === ""}
        className={`bg-primary text-primary-foreground ${ACTION_SIZE} disabled:opacity-50`}
      >
        Send
      </button>
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

  const startSettings = () => {
    setPanel(closedPanel);
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
        startSettings();
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
  const tabsEnabled = view !== null && !archived;

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
        tabs={
          <>
            <PermissionMode
              client={client}
              session={id}
              mode={
                view?.live === true
                  ? transcript.permissionMode
                  : (view?.permission_mode ?? transcript.permissionMode)
              }
              enabled={view !== null && !archived}
            />
            <SettingTab
              label={modelTabLabel(
                models,
                view?.applied_model ?? null,
                picks.model,
                flow.kind !== "closed",
                fallbackModel,
              )}
              stopLabel="Stop here and change model"
              enabled={tabsEnabled}
              picking={flow.kind !== "closed"}
              options={models.map((model) => ({ value: model.value, label: modelOptionLabel(models, model) }))}
              selected={picks.model ?? DEFAULT_MODEL}
              onStop={startSettings}
              onPick={(value) => choosePicks(pickModel(picks, value, models))}
            />
            <SettingTab
              label={effortTabLabel(
                view?.applied_effort ?? null,
                picks.effort,
                flow.kind !== "closed",
                fallbackEffort,
              )}
              stopLabel="Stop here and change effort"
              enabled={tabsEnabled}
              picking={flow.kind !== "closed"}
              options={effortLevels(models, picks.model).map((level) => ({
                value: level,
                label: level,
              }))}
              selected={picks.effort}
              onStop={startSettings}
              onPick={(value) => choosePicks(pickEffort(picks, value))}
            />
            <button
              type="button"
              disabled={!tabsEnabled || flow.kind !== "closed"}
              aria-expanded={panel.kind !== "closed"}
              onClick={() => panelEvent({ type: panel.kind === "closed" ? "open_tab" : "close" })}
              className={`voice-ui pointer-events-auto shrink-0 rounded-t-[5px] border border-b-0 px-2.5 py-1 text-xs whitespace-nowrap shadow-sm disabled:opacity-60 ${panel.kind === "closed" ? "bg-card text-foreground border-border" : "bg-primary text-primary-foreground border-primary"}`}
            >
              /<span className="hidden sm:inline"> Commands</span>
            </button>
            {transcript.turnOpen && (
              <div className="pointer-events-auto ml-auto flex items-end gap-2">
                {stopError !== null && (
                  <span className="bg-background text-destructive mb-1 rounded-[5px] px-2 py-0.5 text-xs shadow-sm">
                    {stopError}
                  </span>
                )}
                <button
                  type="button"
                  onClick={stop}
                  disabled={stopping}
                  className="border-destructive bg-destructive voice-ui w-20 text-black shrink-0 rounded-t-[5px] border border-b-0 py-1 text-xs whitespace-nowrap shadow-sm disabled:opacity-50"
                >
                  {stopping ? "Stopping..." : "Stop"}
                </button>
              </div>
            )}
          </>
        }
        panel={
          panel.kind === "closed" ? undefined : (
            <CommandsPanel
              cards={shownCards}
              argument={argumentCard}
              onUse={(card) => panelEvent({ type: "use", card })}
              onClose={() => panelEvent({ type: "close" })}
            />
          )
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
            <Composer
              session={id}
              onSend={send}
              onSlash={(text) => panelEvent({ type: "open_slash", text })}
            />
          </footer>
        </>
      )}
    </>
  );
}
