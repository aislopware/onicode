import { SERVICE_LAUNCHER_CONTEXT_ENV } from "../service/serviceProtocol.ts";

// Agents inside a managed Onicode service inherit its home and launcher context; CI has neither.
for (const name of ["ONICODE_HOME", "ONICODE_BOOT_SERVICE_UNIT", SERVICE_LAUNCHER_CONTEXT_ENV]) {
  delete process.env[name];
}
