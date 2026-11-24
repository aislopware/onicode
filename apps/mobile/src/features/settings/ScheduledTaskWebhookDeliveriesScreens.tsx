import {
  EnvironmentId,
  ScheduledTaskId,
  ScheduledTaskWebhookDeliveryId,
  type ScheduledTaskWebhookDeliverySummary,
} from "@onicode/contracts";
import { useNavigation, type StaticScreenProps } from "@react-navigation/native";
import type { NativeStackNavigationProp } from "@react-navigation/native-stack";
import { T, useGT, useMessages } from "gt-react-native";
import type { ReactNode } from "react";
import { Pressable, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { SymbolView } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import { RefreshControl } from "../../components/ui/RefreshControl";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { useEnvironmentQuery } from "../../state/query";
import { serverEnvironment } from "../../state/server";
import { useMessageTimeFormatter } from "../threads/useMessageTimeFormatter";
import { SettingsScreen } from "./components/SettingsScreen";
import { SettingsSection } from "./components/SettingsSection";
import { webhookDeliveryFailed, webhookDeliveryLabel } from "./scheduledTaskPresentation";

type DeliveriesParams = { readonly environmentId: string; readonly taskId: string };
type DeliveryParams = DeliveriesParams & { readonly deliveryId: string };

type DeliveryRoutes = {
  SettingsScheduledTaskDelivery: DeliveryParams;
};

function deliveryOutcome(delivery: ScheduledTaskWebhookDeliverySummary) {
  return delivery.pendingWindow ? "coalesced" : delivery.outcome;
}

/** Recent requests to one webhook task, newest first as the server sends them. */
export function SettingsScheduledTaskDeliveriesRouteScreen({
  route,
}: StaticScreenProps<DeliveriesParams>) {
  const gt = useGT();
  const m = useMessages();
  const insets = useSafeAreaInsets();
  const navigation = useNavigation<NativeStackNavigationProp<DeliveryRoutes>>();
  const formatTime = useMessageTimeFormatter();
  const environmentId = EnvironmentId.make(route.params.environmentId);
  const taskId = ScheduledTaskId.make(route.params.taskId);
  const tasks = useEnvironmentQuery(
    serverEnvironment.scheduledTasksLive({ environmentId, input: {} }),
  );
  const task = tasks.data?.tasks.find((candidate) => candidate.id === taskId);
  const deliveries = useEnvironmentQuery(
    serverEnvironment.scheduledTaskWebhookDeliveries({
      environmentId,
      input: { id: taskId, acceptAnyOutcome: true },
    }),
  );
  const list = deliveries.data?.deliveries ?? null;
  const refresh = { accessibilityLabel: gt("Refresh deliveries"), onPress: deliveries.refresh };

  return (
    <SettingsScreen title={gt("Deliveries")} refresh={refresh}>
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-5 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
        refreshControl={
          <RefreshControl
            onRefresh={deliveries.refresh}
            refreshing={deliveries.isPending && list !== null}
            tintColorClassName={String("accent-icon")}
          />
        }
      >
        {tasks.data !== null && task === undefined ? (
          <Text className="px-1 text-base text-danger-foreground">
            <T>This task no longer exists.</T>
          </Text>
        ) : null}
        <SettingsSection
          title={task?.title}
          footer={gt("Recent requests to this task's webhook URL.")}
        >
          {deliveries.error ? (
            <Text className="p-4 text-base text-danger-foreground">{deliveries.error}</Text>
          ) : list === null ? (
            <Text className="p-4 text-base text-foreground-muted">
              <T>Loading deliveries…</T>
            </Text>
          ) : list.length === 0 ? (
            <Text className="p-4 text-base text-foreground-muted">
              <T>No requests yet.</T>
            </Text>
          ) : (
            list.map((delivery, index) => {
              const label = webhookDeliveryLabel(delivery, m);
              const time = formatTime(delivery.receivedAt);
              return (
                <Pressable
                  key={delivery.id}
                  accessibilityRole="button"
                  accessibilityLabel={gt("{outcome}, {method}, {time}", {
                    outcome: label,
                    method: delivery.method,
                    time,
                  })}
                  onPress={() =>
                    navigation.navigate("SettingsScheduledTaskDelivery", {
                      environmentId: route.params.environmentId,
                      taskId: route.params.taskId,
                      deliveryId: String(delivery.id),
                    })
                  }
                  className={
                    index === 0
                      ? "min-h-14 flex-row items-center gap-3 px-4 py-3 active:opacity-70"
                      : "min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70"
                  }
                >
                  <View className="min-w-0 flex-1 gap-1">
                    <Text
                      textRole="body"
                      className={
                        webhookDeliveryFailed(deliveryOutcome(delivery))
                          ? "text-danger-foreground"
                          : "text-foreground"
                      }
                      numberOfLines={1}
                    >
                      {label}
                    </Text>
                    <Text className="text-sm text-foreground-muted" numberOfLines={1}>
                      {gt("{method} · {time}", { method: delivery.method, time })}
                    </Text>
                    {delivery.outcome === "filtered" && delivery.error ? (
                      <Text className="text-sm text-foreground-muted" numberOfLines={2}>
                        {delivery.error}
                      </Text>
                    ) : null}
                    {delivery.missingFields.length > 0 ? (
                      <Text className="text-sm text-foreground-muted" numberOfLines={1}>
                        {gt(
                          "{count, plural, one {{count} empty placeholder} other {{count} empty placeholders}}",
                          { count: delivery.missingFields.length },
                        )}
                      </Text>
                    ) : null}
                  </View>
                  <SymbolView
                    name="chevron.right"
                    size={14}
                    tintColorClassName="accent-chevron"
                    type="monochrome"
                  />
                </Pressable>
              );
            })
          )}
        </SettingsSection>
      </ScrollView>
    </SettingsScreen>
  );
}

function DeliveryBlock({
  title,
  children,
}: {
  readonly title: string;
  readonly children: ReactNode;
}) {
  return (
    <SettingsSection title={title}>
      <Text selectable className="px-4 py-3 font-mono text-sm text-foreground">
        {children}
      </Text>
    </SettingsSection>
  );
}

/** One request: what arrived, what became of it, and the prompt the agent got. */
export function SettingsScheduledTaskDeliveryRouteScreen({
  route,
}: StaticScreenProps<DeliveryParams>) {
  const gt = useGT();
  const m = useMessages();
  const insets = useSafeAreaInsets();
  const formatTime = useMessageTimeFormatter();
  const environmentId = EnvironmentId.make(route.params.environmentId);
  const query = useEnvironmentQuery(
    serverEnvironment.scheduledTaskWebhookDelivery({
      environmentId,
      input: {
        id: ScheduledTaskId.make(route.params.taskId),
        deliveryId: ScheduledTaskWebhookDeliveryId.make(route.params.deliveryId),
        acceptAnyOutcome: true,
      },
    }),
  );
  const delivery = query.data?.delivery ?? null;

  return (
    <SettingsScreen title={gt("Delivery")}>
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-5 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        {query.error ? (
          <Text className="px-1 text-base text-danger-foreground">{query.error}</Text>
        ) : delivery === null ? (
          <Text className="px-1 text-base text-foreground-muted">
            <T>Loading delivery…</T>
          </Text>
        ) : (
          <>
            <SettingsSection>
              <View className="gap-1 px-4 py-3">
                <Text
                  textRole="body"
                  className={
                    webhookDeliveryFailed(deliveryOutcome(delivery))
                      ? "text-danger-foreground"
                      : "text-foreground"
                  }
                >
                  {webhookDeliveryLabel(delivery, m)}
                </Text>
                <Text className="text-sm text-foreground-muted">
                  {gt("{method} · {time}", {
                    method: delivery.method,
                    time: formatTime(delivery.receivedAt),
                  })}
                </Text>
                {delivery.signatureVerified ? (
                  <Text className="text-sm text-foreground-muted">
                    <T>Signature verified</T>
                  </Text>
                ) : null}
                {delivery.error ? (
                  <Text
                    selectable
                    className={
                      delivery.outcome === "filtered"
                        ? "text-sm text-foreground-muted"
                        : "text-sm text-danger-foreground"
                    }
                  >
                    {delivery.error}
                  </Text>
                ) : null}
                {delivery.missingFields.length > 0 ? (
                  <Text className="text-sm text-foreground-muted">
                    {gt("Empty placeholders: {fields}", {
                      fields: delivery.missingFields.join(", "),
                    })}
                  </Text>
                ) : null}
              </View>
            </SettingsSection>
            <DeliveryBlock
              title={delivery.pendingWindow ? gt("Scheduled run") : gt("Prompt sent to the agent")}
            >
              {delivery.renderedPrompt ??
                (delivery.pendingWindow
                  ? gt("Run scheduled when the window closes; no run started yet.")
                  : gt("No run was started for this request."))}
            </DeliveryBlock>
            <DeliveryBlock title={gt("Headers")}>
              {Object.entries(delivery.headers)
                .map(([name, value]) => `${name}: ${value}`)
                .join("\n")}
            </DeliveryBlock>
            {delivery.query ? (
              <DeliveryBlock title={gt("Query", { $context: "URL query string of a request" })}>
                {delivery.query}
              </DeliveryBlock>
            ) : null}
            <DeliveryBlock title={delivery.bodyTruncated ? gt("Body (truncated)") : gt("Body")}>
              {delivery.body || gt("(empty)")}
            </DeliveryBlock>
          </>
        )}
      </ScrollView>
    </SettingsScreen>
  );
}
