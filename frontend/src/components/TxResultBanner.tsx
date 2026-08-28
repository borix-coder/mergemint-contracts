import { SubmitResult } from "../lib/types";
import { explorerTxUrl } from "../lib/network";

export function TxResultBanner({ result }: { result: SubmitResult | null }) {
  if (!result) return null;

  return (
    <div className="tx-result-banner" aria-live="polite">
      Transaction submitted —{" "}
      <a
        href={explorerTxUrl(result.hash, result.network)}
        target="_blank"
        rel="noopener noreferrer"
      >
        view on Stellar Expert
      </a>
    </div>
  );
}
