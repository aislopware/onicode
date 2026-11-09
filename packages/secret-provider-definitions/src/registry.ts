import { makeSecretProviderDefinitionRegistry } from "@onicode/secret-provider-core/shared/definition";
import * as OnePassword from "@onicode/secret-provider-onepassword/shared/definition";

export const builtInSecretProviderDefinitions = makeSecretProviderDefinitionRegistry([
  OnePassword.definition,
]);
