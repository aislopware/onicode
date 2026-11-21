import { useGT } from "gt-react-native";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";

import { useNavigation } from "@react-navigation/native";
import type { EnvironmentId } from "@onicode/contracts";
import { Fragment, useState } from "react";
import { View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { AppBar } from "../../components/ui/AppBar";
import { useRemoteConnections } from "../../state/use-remote-environment-registry";
import { LocalEnvironmentList } from "./LocalEnvironmentList";
import { PullRequestRoutingSettings } from "./PullRequestRoutingSettings";
import { LinkedEnvironmentsSection } from "./LinkedEnvironmentsSection";
import { LinkSeveralEnvironmentsAction } from "./LinkSeveralEnvironments";

export function ConnectionsRouteScreen() {
  const gt = useGT();
  const {
    connectedEnvironments,
    onReconnectEnvironment,
    onRemoveEnvironmentPress,
    onSetEnvironmentEnabled,
    onUpdateEnvironment,
  } = useRemoteConnections();
  const navigation = useNavigation();
  const insets = useSafeAreaInsets();
  const [expandedId, setExpandedId] = useState<EnvironmentId | null>(null);
  // Held here so the sheet outlives its environment disconnecting mid-batch and shows the failure.
  const [linkSeveralFor, setLinkSeveralFor] = useState<EnvironmentId | null>(null);
  const handleToggle = (environmentId: EnvironmentId) => {
    setExpandedId((prev) => (prev === environmentId ? null : environmentId));
  };

  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <AppBar
        title={gt("Environments")}
        onClose={() => navigation.goBack()}
        closeAccessibilityLabel={gt("Close environments")}
        actions={[
          {
            accessibilityLabel: gt("Add environment"),
            icon: "plus",
            onPress: () => navigation.navigate("ConnectionsNew"),
          },
        ]}
      />
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerStyle={{
          paddingBottom: Math.max(insets.bottom, 18) + 18,
          paddingHorizontal: 20,
          paddingTop: 16,
        }}
      >
        <LocalEnvironmentList
          environments={connectedEnvironments}
          expandedId={expandedId}
          onToggle={handleToggle}
          onReconnect={onReconnectEnvironment}
          onRemove={onRemoveEnvironmentPress}
          onSetEnabled={onSetEnvironmentEnabled}
          onUpdate={onUpdateEnvironment}
        />
        {connectedEnvironments.map((environment) => {
          const connected = environment.connectionState === "connected";
          const linkingSeveral = linkSeveralFor === environment.environmentId;
          if (!connected && !linkingSeveral) return null;
          return (
            <Fragment key={environment.environmentId}>
              {connected ? (
                <LinkedEnvironmentsSection
                  environmentId={environment.environmentId}
                  environmentLabel={environment.environmentLabel}
                />
              ) : null}
              <LinkSeveralEnvironmentsAction
                environmentId={environment.environmentId}
                environmentLabel={environment.environmentLabel}
                open={linkingSeveral}
                onOpenChange={(open) => setLinkSeveralFor(open ? environment.environmentId : null)}
              />
            </Fragment>
          );
        })}
        <PullRequestRoutingSettings />
      </ScrollView>
    </View>
  );
}
