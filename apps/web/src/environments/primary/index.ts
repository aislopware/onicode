export {
  PrimaryEnvironmentRequestError,
  resolveInitialServerAuthGateState,
  type ServerClientSessionRecord,
  type ServerPairingLinkRecord,
  __resetServerAuthBootstrapForTests,
} from "./auth";

export {
  DesktopEnvironmentBootstrapIncompleteError,
  readPrimaryEnvironmentTarget,
  resolvePrimaryEnvironmentHttpUrl,
  type PrimaryEnvironmentTarget,
} from "./target";
