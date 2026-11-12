import type { EnvironmentId } from "@onicode/contracts";
import { createFileRoute } from "@tanstack/react-router";

import { RemoteDesktopPage } from "../components/remoteDesktop/RemoteDesktopPage";

export const Route = createFileRoute("/_chat/remote-desktop/$environmentId")({
  component: RemoteDesktopRoute,
});

function RemoteDesktopRoute() {
  const { environmentId } = Route.useParams();
  return <RemoteDesktopPage key={environmentId} environmentId={environmentId as EnvironmentId} />;
}
