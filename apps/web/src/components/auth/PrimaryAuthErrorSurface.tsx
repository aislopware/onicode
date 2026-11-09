import { Alert, AlertDescription } from "../ui/alert";
import { Button } from "../ui/button";
import { StandalonePage, StandalonePageHeader } from "../ui/standalone-page";

export function PrimaryAuthErrorSurface({ errorMessage }: { readonly errorMessage: string }) {
  return (
    <StandalonePage>
      <StandalonePageHeader
        title="Onicode could not sign in to its own server."
        description="The desktop app's credential for the server on this machine was not accepted. Reloading usually fixes this; if it does not, quit and reopen Onicode."
      />
      <div className="mt-5">
        <Alert variant="error">
          <AlertDescription>{errorMessage}</AlertDescription>
        </Alert>
      </div>
      <div className="mt-5 flex flex-wrap gap-2">
        <Button size="sm" onClick={() => window.location.reload()}>
          Reload app
        </Button>
      </div>
    </StandalonePage>
  );
}
