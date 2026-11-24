import { agentSignInFailureMessage } from "@onicode/client-runtime/agent-sign-in-failure";
import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import { useAtomValue } from "@effect/atom-react";
import {
  AuthAccessWriteScope,
  type AuthMcpClientAccess,
  type EnvironmentId,
  mcpApprovalCode,
} from "@onicode/contracts";
import { CheckIcon, CopyIcon } from "lucide-react";
import * as DateTime from "effect/DateTime";
import * as Redacted from "effect/Redacted";
import { Atom } from "effect/reactivity";
import { msg, T, useGT, useMessages } from "gt-react";
import { type ReactNode, useState } from "react";

import {
  namedClipboardTarget,
  useCopyToClipboard,
  writeTextToClipboard,
} from "~/hooks/useCopyToClipboard";
import { useNowSecond } from "~/hooks/useNowSecond";
import { useUpdateEnvironmentSettings } from "~/hooks/useSettings";
import { m } from "~/i18n/messages";
import { readLocalApi } from "~/localApi";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { authEnvironment } from "~/state/auth";
import {
  type EnvironmentPresentation,
  useEnvironment,
  useEnvironmentHttpBaseUrl,
  usePrimaryEnvironmentId,
} from "~/state/environments";
import { serverEnvironment } from "~/state/server";
import { useManagedPolicy } from "~/state/managedPolicy";
import { useEnvironmentScope } from "~/state/session";
import { useAtomCommand } from "~/state/use-atom-command";
import { CommandBlock } from "../CommandBlock";
import { usePublicAccess } from "../settings/publicAccess";
import {
  publicAccessStatusText,
  publicAccessSwitchDenial,
} from "@onicode/client-runtime/public-access-status";
import { Button } from "../ui/button";
import { Dialog } from "../ui/dialog";
import { Input } from "../ui/input";
import { toastManager } from "../ui/toast";
import { WizardFooter, WizardHeader, WizardPanel, WizardPopup, WizardSteps } from "../ui/wizard";
import {
  AgentAccessPicker,
  closestGrantable,
  DEFAULT_AGENT_ACCESS,
  useGrantableAgentAccess,
} from "./AgentAccessPicker";
import {
  MCP_APPS,
  type McpApp,
  mcpHandoffAction,
  OTHER_MCP_APP_STEPS,
  signInWorksAt,
} from "./mcpApps";

const requestAtom = Atom.make<{ readonly environmentId: EnvironmentId | null } | null>(null).pipe(
  Atom.keepAlive,
  Atom.withLabel("mcp:use-in-another-app"),
);

/** Opens the dialog for `environmentId`, or for the primary environment when null. */
export function openUseInAnotherApp(environmentId: EnvironmentId | null): void {
  appAtomRegistry.set(requestAtom, { environmentId });
}

const close = () => appAtomRegistry.set(requestAtom, null);

/** Hosted once at the root, so Settings and the command palette can open it from anywhere. */
export function UseInAnotherAppDialogHost() {
  const request = useAtomValue(requestAtom);
  return request === null ? null : <UseInAnotherAppDialog requested={request.environmentId} />;
}

function UseInAnotherAppDialog({ requested }: { readonly requested: EnvironmentId | null }) {
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const environment = useEnvironment(requested ?? primaryEnvironmentId);
  const gt = useGT();
  const title = `${gt("Use Onicode in another app (MCP)")} · ${environment?.label ?? "Onicode"}`;
  return (
    <Dialog open onOpenChange={(open) => (open ? undefined : close())}>
      <WizardPopup>
        {environment !== null &&
        environment.connection.phase === "connected" &&
        environment.serverConfig?.environment.capabilities.webhookSchedules !== true ? (
          <>
            <WizardHeader title={title} />
            <WizardPanel>
              <p className="text-sm text-muted-foreground">
                <T>Update Onicode on this environment to connect other apps.</T>
              </p>
            </WizardPanel>
          </>
        ) : environment !== null && environment.connection.phase === "connected" ? (
          <Wizard key={environment.environmentId} environment={environment} title={title} />
        ) : (
          <>
            <WizardHeader title={title} />
            <WizardPanel>
              <p className="text-sm text-muted-foreground">
                <T>Connect to the environment first.</T>
              </p>
            </WizardPanel>
          </>
        )}
      </WizardPopup>
    </Dialog>
  );
}

const STEPS = [
  msg("App", { $context: "wizard step: choose the app" }),
  msg("Connect", { $context: "wizard step: connect the app" }),
] as const;
type Choice = McpApp | "other";

function Wizard({
  environment,
  title,
}: {
  readonly environment: EnvironmentPresentation;
  readonly title: string;
}) {
  const m = useMessages();
  const [step, setStep] = useState<0 | 1>(0);
  const [choice, setChoice] = useState<Choice | null>(null);
  return (
    <>
      <WizardHeader title={title}>
        <WizardSteps
          steps={STEPS.map((label) => m(label))}
          currentStep={step}
          isStepDisabled={(index) => index > step}
          onStepChange={(index) => setStep(index === 0 ? 0 : 1)}
        />
      </WizardHeader>
      <WizardPanel>
        {step === 0 || choice === null ? (
          <AppStep choice={choice} onChoose={setChoice} />
        ) : (
          <ConnectStep environment={environment} choice={choice} />
        )}
      </WizardPanel>
      <WizardFooter>
        {step === 1 ? (
          <>
            <Button variant="outline" onClick={() => setStep(0)}>
              <T>Back</T>
            </Button>
            <Button onClick={close}>
              <T>Done</T>
            </Button>
          </>
        ) : (
          <Button disabled={choice === null} onClick={() => setStep(1)}>
            <T>Next</T>
          </Button>
        )}
      </WizardFooter>
    </>
  );
}

function AppLogo({ app }: { readonly app: McpApp }) {
  if (typeof app.logo === "string") {
    return (
      <img
        src={app.logo}
        alt=""
        className="size-10 rounded-lg ring-1 ring-black/10 dark:ring-white/12"
      />
    );
  }
  const Logo = app.logo;
  return (
    <span className="grid size-10 place-items-center rounded-lg bg-card ring-1 ring-black/10 dark:ring-white/12">
      <Logo aria-hidden className="size-6" />
    </span>
  );
}

function AppStep({
  choice,
  onChoose,
}: {
  readonly choice: Choice | null;
  readonly onChoose: (choice: Choice) => void;
}) {
  return (
    <section className="space-y-3">
      <h3 className="text-sm font-medium">
        <T>Which app?</T>
      </h3>
      <div className="grid grid-cols-3 gap-2 sm:grid-cols-5">
        {MCP_APPS.map((app) => (
          <button
            key={app.id}
            type="button"
            aria-pressed={choice === app}
            onClick={() => onChoose(app)}
            className="flex min-w-0 flex-col items-center gap-2 rounded-lg border border-transparent px-1 py-3 outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring aria-pressed:border-primary aria-pressed:bg-primary/8"
          >
            <AppLogo app={app} />
            <span className="max-w-full truncate whitespace-nowrap text-xs font-medium">
              {app.name}
            </span>
          </button>
        ))}
      </div>
      <button
        type="button"
        aria-pressed={choice === "other"}
        onClick={() => onChoose("other")}
        className="w-full rounded-lg border border-border px-3 py-2.5 text-sm font-medium outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring aria-pressed:border-primary aria-pressed:bg-primary/8"
      >
        <T>Something else…</T>
      </button>
    </section>
  );
}

function ConnectStep({
  environment,
  choice,
}: {
  readonly environment: EnvironmentPresentation;
  readonly choice: Choice;
}) {
  const gt = useGT();
  const m = useMessages();
  const app = choice === "other" ? null : choice;
  const environmentId = environment.environmentId;
  const publicAccess = usePublicAccess(environmentId);
  const baseUrl = useEnvironmentHttpBaseUrl(environmentId);
  const deviceUrl = baseUrl === null ? null : `${baseUrl.replace(/\/+$/u, "")}/mcp`;
  const [anotherWay, setAnotherWay] = useState(app === null);
  // A cloud app reaches only a public address; anything else can use this device's when an agent
  // can sign in there.
  const usableDeviceUrl = deviceUrl !== null && signInWorksAt(deviceUrl) ? deviceUrl : null;
  const mcpUrl =
    app?.reach === "cloud" ? publicAccess.mcpUrl : (usableDeviceUrl ?? publicAccess.mcpUrl);
  if (mcpUrl === null) {
    const httpOnlyUrl = app?.reach !== "cloud" ? deviceUrl : null;
    return (
      <section className="space-y-4">
        <PublicAgentsGate environmentId={environmentId} app={app} httpOnly={httpOnlyUrl !== null} />
        {/* A token needs no sign-in, so it works over plain http too. */}
        {httpOnlyUrl === null ? null : (
          <AgentToken environment={environment} mcpUrl={httpOnlyUrl} />
        )}
      </section>
    );
  }
  const approve = (
    <p className="text-sm text-muted-foreground">
      {app === null
        ? gt(
            "When the app opens its sign-in page, approve it in Onicode on any of your devices, and check that both show the same code.",
          )
        : gt(
            "When {app} opens its sign-in page, approve it in Onicode on any of your devices, and check that both show the same code.",
            { app: app.name },
          )}
    </p>
  );
  const publicLine =
    app?.reach !== "cloud" && publicAccess.mcpUrl !== null && publicAccess.mcpUrl !== mcpUrl ? (
      <CopyRow label={gt("Public URL")} value={publicAccess.mcpUrl} />
    ) : null;
  if (anotherWay || app === null) {
    const message = app?.handoff.kind === "copy-message" ? app.handoff.message(mcpUrl) : null;
    return (
      <section className="space-y-4">
        <CopyRow label={gt("URL")} value={mcpUrl} />
        {publicLine}
        {message ? (
          <CopyRow label={gt("Message", { $context: "message to send" })} value={message} />
        ) : null}
        <ol className="list-decimal space-y-1 ps-5 text-sm">
          {(app?.steps ?? OTHER_MCP_APP_STEPS).map((step) => (
            <li key={step}>{m(step)}</li>
          ))}
        </ol>
        {approve}
        <AgentCode environment={environment} />
        <AgentToken environment={environment} mcpUrl={mcpUrl} />
        {app === null ? null : (
          <TextButton onClick={() => setAnotherWay(false)}>
            {app.handoff.kind === "commands" ? gt("Back to commands") : gt("Back to one click")}
          </TextButton>
        )}
      </section>
    );
  }
  return (
    <section className="space-y-4">
      <PrimaryAction app={app} mcpUrl={mcpUrl} />
      <CopyRow label={gt("URL")} value={mcpUrl} />
      {publicLine}
      {approve}
      <TextButton onClick={() => setAnotherWay(true)}>
        <T>Set up another way</T>
      </TextButton>
    </section>
  );
}

/** Cloud apps call from the internet, so they need public agents on; this turns it on here. */
function PublicAgentsGate({
  environmentId,
  app,
  httpOnly,
}: {
  readonly environmentId: EnvironmentId;
  readonly app: McpApp | null;
  /** This device reaches the environment over plain http, where agents cannot sign in. */
  readonly httpOnly: boolean;
}) {
  const gt = useGT();
  const m = useMessages();
  const { agents, status } = usePublicAccess(environmentId);
  const canChange = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const updateSettings = useUpdateEnvironmentSettings(environmentId);
  const denial = publicAccessSwitchDenial(
    useManagedPolicy(environmentId),
    "publicAgentsEnabled",
    false,
    m,
  );
  const statusText = status === null ? null : publicAccessStatusText(status, m);
  return (
    <section className="space-y-3">
      <h3 className="text-sm font-medium">
        {httpOnly
          ? gt("Agents sign in only over https")
          : app === null
            ? gt("The app needs a public address")
            : gt("{app} needs a public address", { app: app.name })}
      </h3>
      <p className="text-sm text-muted-foreground">
        {httpOnly ? (
          <T>
            This device reaches the environment over plain http, where agents cannot finish signing
            in. Use Tailscale HTTPS in Settings → Sharing &amp; access, run the agent on the
            computer running Onicode, or turn on public agents.
          </T>
        ) : (
          <T>
            Cloud apps reach Onicode from the internet. Public agents publishes only /mcp and its
            sign-in through Tailscale Funnel; every app still needs your approval. The tailnet needs
            HTTPS certificates and the funnel node attribute.
          </T>
        )}
      </p>
      {agents ? null : (
        <Button
          disabled={!canChange || denial !== undefined}
          onClick={() => void updateSettings({ publicAgentsEnabled: true })}
        >
          <T>Turn on public agents</T>
        </Button>
      )}
      {denial !== undefined && !agents ? (
        <p className="text-xs text-muted-foreground">{denial}</p>
      ) : !canChange && !agents ? (
        <p className="text-xs text-muted-foreground">
          <T>This device cannot change this environment's settings.</T>
        </p>
      ) : null}
      {statusText ? (
        <p
          className={
            status?._tag === "failed"
              ? "text-sm text-destructive-foreground"
              : "text-sm text-muted-foreground"
          }
        >
          {statusText}
        </p>
      ) : null}
    </section>
  );
}

function TextButton({
  onClick,
  children,
}: {
  readonly onClick: () => void;
  readonly children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="block text-sm font-medium underline-offset-4 outline-none hover:underline focus-visible:underline"
    >
      {children}
    </button>
  );
}

function openLink(url: string) {
  const api = readLocalApi();
  if (api === undefined) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  void api.shell.openExternal(url).catch((error: unknown) =>
    toastManager.add({
      type: "error",
      title: m(msg("Could not open the link")),
      description: thrownErrorMessage(error, m) ?? url,
    }),
  );
}

function PrimaryAction({ app, mcpUrl }: { readonly app: McpApp; readonly mcpUrl: string }) {
  const gt = useGT();
  const m = useMessages();
  const action = mcpHandoffAction(app, mcpUrl, m);
  const note = app.note ? <p className="text-xs text-muted-foreground">{m(app.note)}</p> : null;
  if (action === null) {
    return (
      <div className="space-y-2">
        {app.handoff.kind === "commands"
          ? app.handoff
              .commands(mcpUrl)
              .map((command) => <CommandBlock key={command} command={command} />)
          : null}
        <p className="text-xs text-muted-foreground">
          {gt("Run these where {app} runs.", { app: app.name })}
        </p>
        {note}
      </div>
    );
  }
  const handOff = async () => {
    if (action.copy !== null) {
      // The app opens only once the copy lands, so it never gets an older clipboard.
      const copied = await writeTextToClipboard(
        action.copy.text,
        namedClipboardTarget(action.copy.title),
      ).catch(() => false);
      if (!copied) {
        toastManager.add({
          type: "error",
          title: gt("Could not copy"),
          description: gt("Use Set up another way."),
        });
        return;
      }
      toastManager.add({ type: "success", title: action.copy.title });
    }
    openLink(action.link);
  };
  return (
    <div className="space-y-2">
      <Button className="w-full" onClick={() => void handOff()}>
        {action.label}
      </Button>
      {note}
    </div>
  );
}

/** "Get code" mints on click; a code is shown once and never copied by itself. */
function AgentCode({ environment }: { readonly environment: EnvironmentPresentation }) {
  const gt = useGT();
  const denied = useGrantableAgentAccess(environment.environmentId, environment.label);
  const [picked, setPicked] = useState<AuthMcpClientAccess | null>(null);
  const access = picked ?? closestGrantable(DEFAULT_AGENT_ACCESS, denied);
  const [code, setCode] = useState<{ readonly value: string; readonly expiresAt: number } | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mint = useAtomCommand(authEnvironment.createPairingCredential, { reportFailure: false });
  const now = useNowSecond();
  if (access === null) return null;
  const live = code !== null && now < code.expiresAt;
  const left = code === null ? 0 : Math.max(0, Math.floor((code.expiresAt - now) / 1000));
  const getCode = async () => {
    setPending(true);
    setError(null);
    const result = await mint({
      environmentId: environment.environmentId,
      input: { access, purpose: "agent", label: gt("Agent code") },
    });
    setPending(false);
    if (result._tag === "Success") {
      setCode({
        value: mcpApprovalCode(access, result.value.credential),
        expiresAt: DateTime.toEpochMillis(result.value.expiresAt),
      });
    } else {
      setError(
        gt("Could not get a code from {environment}. Try again.", {
          environment: environment.label,
        }),
      );
    }
  };
  return (
    <details className="group rounded-lg border border-border px-3 py-2">
      <summary className="cursor-pointer list-none text-sm font-medium outline-none focus-visible:underline [&::-webkit-details-marker]:hidden">
        <T>No Onicode device at hand? Approve with a code</T>
      </summary>
      <div className="mt-3 space-y-3">
        <p className="text-xs text-muted-foreground">
          <T>Enter the code on the app's sign-in page. It sets what the app may do.</T>
        </p>
        <AgentAccessPicker
          label={gt("What it may do")}
          value={access}
          denied={denied}
          onChange={(next) => {
            setPicked(next);
            setCode(null);
          }}
          disabled={pending}
        />
        {live ? (
          <>
            <CopyRow label={gt("Code", { $context: "approval code" })} value={code.value} />
            <p className="text-xs text-muted-foreground">
              {gt("Expires {time} · one use", {
                time: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`,
              })}
            </p>
          </>
        ) : (
          <Button size="sm" variant="outline" disabled={pending} onClick={() => void getCode()}>
            {pending ? gt("Getting code…") : code === null ? gt("Get code") : gt("New code")}
          </Button>
        )}
        {error ? <p className="text-xs text-destructive-foreground">{error}</p> : null}
      </div>
    </details>
  );
}

/** For apps that send a fixed Authorization header instead of signing in. */
function AgentToken({
  environment,
  mcpUrl,
}: {
  readonly environment: EnvironmentPresentation;
  readonly mcpUrl: string;
}) {
  const gt = useGT();
  const denied = useGrantableAgentAccess(environment.environmentId, environment.label);
  const [picked, setPicked] = useState<AuthMcpClientAccess | null>(null);
  const access = picked ?? closestGrantable(DEFAULT_AGENT_ACCESS, denied);
  const [label, setLabel] = useState("");
  const [token, setToken] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const issue = useAtomCommand(serverEnvironment.issueAgentToken, { reportFailure: false });
  if (access === null) return null;
  const create = async () => {
    setPending(true);
    setError(null);
    const result = await issue({
      environmentId: environment.environmentId,
      input: { label: label.trim(), access },
    });
    setPending(false);
    if (result._tag === "Success") setToken(Redacted.value(result.value.token));
    else setError(agentSignInFailureMessage(squashAtomCommandFailure(result), m));
  };
  return (
    <details className="group rounded-lg border border-border px-3 py-2">
      <summary className="cursor-pointer list-none text-sm font-medium outline-none focus-visible:underline [&::-webkit-details-marker]:hidden">
        <T>App without sign-in? Create a token</T>
      </summary>
      <div className="mt-3 space-y-3">
        {token === null ? (
          <>
            <p className="text-xs text-muted-foreground">
              <T>
                The app sends it as a bearer token. It works for 30 days until revoked under
                Authorized clients.
              </T>
            </p>
            <Input
              value={label}
              onChange={(event) => setLabel(event.currentTarget.value)}
              placeholder={gt("Name, such as Nightly triage")}
              aria-label={gt("Token name")}
              disabled={pending}
            />
            <AgentAccessPicker
              label={gt("What it may do")}
              value={access}
              denied={denied}
              onChange={setPicked}
              disabled={pending}
            />
            <Button
              size="sm"
              variant="outline"
              disabled={pending || label.trim() === ""}
              onClick={() => void create()}
            >
              {pending ? gt("Creating…") : gt("Create token")}
            </Button>
          </>
        ) : (
          <>
            <CopyRow label={gt("Token")} value={token} />
            <CopyRow label={gt("Header")} value={`Authorization: Bearer ${token}`} />
            <CopyRow label={gt("URL")} value={mcpUrl} />
            <p className="text-xs text-muted-foreground">
              <T>Copy it now; Onicode does not show it again.</T>
            </p>
          </>
        )}
        {error ? <p className="text-xs text-destructive-foreground">{error}</p> : null}
      </div>
    </details>
  );
}

function CopyRow({ label, value }: { readonly label: string; readonly value: string }) {
  const { copyToClipboard, isCopied } = useCopyToClipboard({
    timeout: 1500,
    target: namedClipboardTarget(label),
  });
  const gt = useGT();
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-lg border border-border bg-muted/50 px-3 py-2">
      <span className="shrink-0 text-xs text-muted-foreground">{label}</span>
      <span className="min-w-0 flex-1 truncate font-mono text-sm">{value}</span>
      <Button
        size="icon-xs"
        variant="ghost"
        aria-label={gt("Copy {label}", { label })}
        onClick={() => copyToClipboard(value, undefined)}
      >
        {isCopied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
      </Button>
    </div>
  );
}
