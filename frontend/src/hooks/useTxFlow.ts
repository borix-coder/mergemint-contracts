import { useState } from "react";
import { SubmitResult, NetworkName } from "../lib/types";

interface TxFlowState {
  pending: boolean;
  error: string | null;
  result: SubmitResult | null;
}

export interface UseTxFlowResult {
  pending: boolean;
  error: string | null;
  result: SubmitResult | null;
  run: (
    submit: () => Promise<{ hash: string; ledger?: number }>,
  ) => Promise<SubmitResult>;
}

export function useTxFlow(network: NetworkName): UseTxFlowResult {
  const [state, setState] = useState<TxFlowState>({
    pending: false,
    error: null,
    result: null,
  });

  async function run(submit: () => Promise<{ hash: string; ledger?: number }>) {
    setState({ pending: true, error: null, result: null });
    try {
      const { hash, ledger } = await submit();
      const result: SubmitResult = { hash, network, ledger };
      setState({ pending: false, error: null, result });
      return result;
    } catch (err) {
      const message = err instanceof Error ? err.message : "Transaction failed";
      setState({ pending: false, error: message, result: null });
      throw err;
    }
  }

  return { ...state, run };
}
