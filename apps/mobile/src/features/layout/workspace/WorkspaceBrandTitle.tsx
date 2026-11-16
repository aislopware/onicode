import type { EnvironmentId } from "@onicode/contracts";
import { CompactBrandTitle } from "../../../components/CompactBrandTitle";
import { WorkspaceConnectionTitle } from "../../home/WorkspaceConnectionTitle";

/** The title of a workspace list tab: the brand and the connection status. */
export function WorkspaceBrandTitle(props: {
  readonly accessibilityLabel: string;
  readonly onOpenEnvironments: (environmentId?: EnvironmentId) => void;
}) {
  return (
    <WorkspaceConnectionTitle
      grow
      onPress={props.onOpenEnvironments}
      brand={
        <CompactBrandTitle
          allowFontScaling={false}
          accessibilityLabel={props.accessibilityLabel}
          fit
        />
      }
    />
  );
}
