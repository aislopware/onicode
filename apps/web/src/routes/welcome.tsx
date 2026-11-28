import { createFileRoute, useLocation, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { EnvironmentId } from "@onicode/contracts";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import { NoProjectsHero } from "../components/NoProjectsHero";
import { WelcomePage } from "../components/onboarding/WelcomePage";
import { useNewThreadHandler } from "../hooks/useHandleNewThread";
import { useEnvironments, usePrimaryEnvironmentId } from "../state/environments";

const decodeEnvironmentId = Schema.decodeOption(EnvironmentId);

export const Route = createFileRoute("/welcome")({
  component: WelcomeRouteView,
});

function WelcomeRouteView() {
  const navigate = useNavigate();
  const hash = useLocation({ select: (location) => location.hash });
  const reviewProjects = hash.startsWith("projects:");
  // A project import shortcut carries the computer it was opened for.
  const importEnvironmentId = reviewProjects
    ? Option.getOrUndefined(decodeEnvironmentId(hash.slice("projects:".length)))
    : undefined;
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const { environments, isReady } = useEnvironments();
  // Trust the id only once the catalog has it.
  const waitingForCatalog = !isReady && (reviewProjects || primaryEnvironmentId === null);
  // An import shortcut must never silently switch to a different computer.
  const environmentId = reviewProjects
    ? (environments.find((environment) => environment.environmentId === importEnvironmentId)
        ?.environmentId ?? null)
    : primaryEnvironmentId;
  // The root can remount this pending outlet after navigation; do not reopen setup.
  const isWelcomeRoute = useLocation({ select: (location) => location.pathname === "/welcome" });
  const [dismissed, setDismissed] = useState(false);
  const openNewThread = useNewThreadHandler();
  if (waitingForCatalog) return null;
  if (environmentId === null || !isWelcomeRoute || dismissed) return <NoProjectsHero />;
  return (
    <WelcomePage
      key={environmentId}
      environmentId={environmentId}
      reviewProjects={reviewProjects}
      onDone={async (projectRef) => {
        setDismissed(true);
        if (projectRef !== undefined) {
          await openNewThread(projectRef, { replace: true }).catch(() =>
            navigate({ to: "/", replace: true }),
          );
          return;
        }
        await navigate({ to: "/", replace: true });
      }}
    />
  );
}
