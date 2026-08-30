import React, { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import { api } from '../lib/api';
import { Contributor } from '../types';
import { mapErrorMessage } from '../utils/format';

export function ContributorProfile() {
  const { address } = useParams<{ address: string }>();
  const [contributor, setContributor] = useState<Contributor | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!address) return;
    api
      .getContributor(address)
      .then(setContributor)
      .catch((err) => setError(mapErrorMessage(err instanceof Error ? err.message : String(err))));
  }, [address]);

  if (error) return <p role="alert">{error}</p>;
  if (!contributor) return <p>Loading...</p>;

  return (
    <div>
      <h1>{contributor.address}</h1>
      <p>Reputation: {contributor.reputation}</p>
      <p>Completed bounties: {contributor.completedBounties}</p>
    </div>
  );
}
