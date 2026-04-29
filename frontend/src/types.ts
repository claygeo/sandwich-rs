export type SandwichRow = {
  id: string;
  victim_sig: string;
  front_sig: string;
  back_sig: string;
  attacker_signer: string;
  victim_signer: string;
  pool: string;
  dex: string;
  slot_span: number;
  profit_lamports: number | null;
  profit_sol: number | null;
  profit_usd: number | null;
  confidence: number;
  detected_at: string;
};

export type Stats24h = {
  sandwich_count: number;
  total_profit_sol: number;
  total_profit_usd: number;
  unique_attackers: number;
  unique_victim_pools: number;
  delta_pct: number | null;
  computed_at: string;
};

export type ConnectionState = "connecting" | "healthy" | "degraded" | "disconnected";
