import type { ReactNode } from "react";
import { View } from "react-native";

import { AppText, AppTextInput, type AppTextInputProps } from "../../components/ui/AppText";
import { cn } from "../../lib/cn";

type ConnectionFormFieldProps = Omit<AppTextInputProps, "accessibilityLabel" | "className"> & {
  readonly label: string;
  readonly className?: string;
};

export function ConnectionFormField({ label, className, ...inputProps }: ConnectionFormFieldProps) {
  return (
    <LabeledConnectionField label={label} className={className}>
      <AppTextInput
        {...inputProps}
        accessibilityLabel={label}
        className="rounded-[14px] px-4 py-3.5"
      />
    </LabeledConnectionField>
  );
}

/** The field's own input carries the label for screen readers. */
export function LabeledConnectionField(props: {
  readonly label: string;
  readonly className?: string | undefined;
  readonly children: ReactNode;
}) {
  return (
    <View collapsable={false} className={cn("gap-1.5", props.className)}>
      <AppText
        accessibilityElementsHidden
        importantForAccessibility="no-hide-descendants"
        className="text-2xs font-oni-bold tracking-[0.8px] uppercase text-foreground-muted"
      >
        {props.label}
      </AppText>
      {props.children}
    </View>
  );
}
