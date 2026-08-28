import { useState } from "react";
import { useTxFlow } from "../hooks/useTxFlow";
import { TxResultBanner } from "./TxResultBanner";
import { CharCounter } from "./CharCounter";
import { NetworkName } from "../lib/types";
import { SYMBOL_MAX_LENGTH } from "../lib/validation";

interface CreateBountyProps {
  network: NetworkName;
  onSubmit: (form: {
    title: string;
    description: string;
    rewardAmount: string;
    maxAssignees: number;
    verifiers: string[];
    threshold: number;
  }) => Promise<{ hash: string }>;
}

export function CreateBounty({ network, onSubmit }: CreateBountyProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [rewardAmount, setRewardAmount] = useState("");
  const [multisigOpen, setMultisigOpen] = useState(false);
  const [verifiers, setVerifiers] = useState<string[]>([""]);
  const [threshold, setThreshold] = useState(1);
  const [maxAssignees, setMaxAssignees] = useState(1);
  const { pending, error, result, run } = useTxFlow(network);

  async function perform() {
    await run(() =>
      onSubmit({
        title,
        description,
        rewardAmount,
        maxAssignees,
        verifiers: verifiers.filter((v) => v.trim() !== ""),
        threshold,
      })
    );
  }

  return (
    <form
      className="create-bounty"
      onSubmit={(e) => {
        e.preventDefault();
        perform();
      }}
    >
      <label>
        Title
        <input
          value={title}
          maxLength={SYMBOL_MAX_LENGTH}
          onChange={(e) => setTitle(e.target.value)}
        />
        <CharCounter length={title.length} max={SYMBOL_MAX_LENGTH} />
      </label>
      <label>
        Description
        <textarea
          value={description}
          maxLength={SYMBOL_MAX_LENGTH}
          onChange={(e) => setDescription(e.target.value)}
        />
        <CharCounter length={description.length} max={SYMBOL_MAX_LENGTH} />
      </label>
      <label>
        Reward amount
        <input value={rewardAmount} onChange={(e) => setRewardAmount(e.target.value)} />
      </label>

      <label>
        Max assignees
        <input
          type="number"
          min={1}
          value={maxAssignees}
          onChange={(e) => setMaxAssignees(Number(e.target.value))}
        />
        <span className="create-bounty__hint">
          When more than one assignee is allowed, the reward is split across claimants by share.
        </span>
      </label>

      <details
        className="create-bounty__advanced"
        open={multisigOpen}
        onToggle={(e) => setMultisigOpen((e.target as HTMLDetailsElement).open)}
      >
        <summary>Advanced: multi-sig verifiers</summary>

        {verifiers.map((verifier, i) => (
          <div className="verifier-row" key={i}>
            <input
              placeholder="Verifier address"
              value={verifier}
              onChange={(e) => {
                const next = [...verifiers];
                next[i] = e.target.value;
                setVerifiers(next);
              }}
            />
            <button
              type="button"
              onClick={() => setVerifiers(verifiers.filter((_, idx) => idx !== i))}
              disabled={verifiers.length <= 1}
            >
              Remove
            </button>
          </div>
        ))}
        <button type="button" onClick={() => setVerifiers([...verifiers, ""])}>
          Add verifier
        </button>

        <label>
          Approval threshold
          <select value={threshold} onChange={(e) => setThreshold(Number(e.target.value))}>
            {verifiers.map((_, i) => (
              <option key={i} value={i + 1}>
                {i + 1} of {verifiers.length}
              </option>
            ))}
          </select>
        </label>
      </details>

      <button type="submit" disabled={pending}>
        {pending ? "Submitting…" : "Create bounty"}
      </button>

      {error && <p className="error">{error}</p>}
      <TxResultBanner result={result} />
    </form>
  );
}
