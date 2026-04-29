import { useCallback, useEffect, useMemo, useState } from "react";
import { Sidebar } from "./components/Sidebar";
import { HeroStatBand } from "./components/HeroStatBand";
import { FeedTable } from "./components/FeedTable";
import { LeaderboardTable } from "./components/LeaderboardTable";
import { Methodology } from "./pages/Methodology";
import { supabase, isSupabaseConfigured } from "./lib/supabase";
import type { ConnectionState, SandwichRow, Stats24h } from "./types";

type Route = "feed" | "leaderboard" | "methodology";

const FEED_LIMIT = 100;

export function App() {
  const [route, setRoute] = useState<Route>(initialRoute());
  const [rows, setRows] = useState<SandwichRow[]>([]);
  const [stats, setStats] = useState<Stats24h | null>(null);
  const [connection, setConnection] = useState<ConnectionState>("connecting");

  // Sync route to URL hash so links work + back button doesn't lie.
  useEffect(() => {
    const onHash = () => setRoute(initialRoute());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  const navigate = useCallback((r: Route) => {
    window.location.hash = r === "feed" ? "" : r;
    setRoute(r);
  }, []);

  // Initial fetch + realtime subscription
  useEffect(() => {
    if (!isSupabaseConfigured) {
      setConnection("disconnected");
      return;
    }
    let mounted = true;

    async function bootstrap() {
      const { data: feedRows, error: feedErr } = await supabase
        .from("sandwich_attempts")
        .select(
          "id, victim_sig, front_sig, back_sig, attacker_signer, victim_signer, pool, dex, slot_span, profit_lamports, profit_sol, profit_usd, confidence, detected_at"
        )
        .order("detected_at", { ascending: false })
        .limit(FEED_LIMIT);
      if (feedErr) console.error("[feed initial]", feedErr);
      if (mounted && feedRows) setRows(feedRows as SandwichRow[]);

      const { data: stats24h, error: statsErr } = await supabase
        .from("stats_24h")
        .select(
          "sandwich_count, total_profit_sol, total_profit_usd, unique_attackers, unique_victim_pools, delta_pct, computed_at"
        )
        .eq("id", 1)
        .maybeSingle();
      if (statsErr) console.error("[stats initial]", statsErr);
      if (mounted && stats24h) setStats(stats24h as Stats24h);

      setConnection("connecting");
    }

    bootstrap();

    // Realtime channel
    const channel = supabase
      .channel("sandwich-attempts-stream")
      .on(
        "postgres_changes",
        { event: "INSERT", schema: "public", table: "sandwich_attempts" },
        (payload) => {
          const fresh = payload.new as SandwichRow;
          setRows((prev) => {
            if (prev.some((r) => r.id === fresh.id)) return prev;
            return [fresh, ...prev].slice(0, FEED_LIMIT);
          });
        }
      )
      .subscribe((status) => {
        if (status === "SUBSCRIBED") setConnection("healthy");
        else if (status === "CHANNEL_ERROR" || status === "TIMED_OUT")
          setConnection("degraded");
        else if (status === "CLOSED") setConnection("disconnected");
      });

    // Periodic stats refresh
    const statsTimer = window.setInterval(async () => {
      const { data } = await supabase
        .from("stats_24h")
        .select(
          "sandwich_count, total_profit_sol, total_profit_usd, unique_attackers, unique_victim_pools, delta_pct, computed_at"
        )
        .eq("id", 1)
        .maybeSingle();
      if (mounted && data) setStats(data as Stats24h);
    }, 60_000);

    return () => {
      mounted = false;
      window.clearInterval(statsTimer);
      supabase.removeChannel(channel);
    };
  }, []);

  const topRightStatus = useMemo(() => {
    if (!stats) return "Mainnet · loading…";
    return `Mainnet · 24h: ${formatHeroBadge(stats.total_profit_usd ?? 0)}`;
  }, [stats]);

  return (
    <div className="app">
      <Sidebar route={route} onNavigate={navigate} connection={connection} />
      <main className="main">
        <div className="top-status">{topRightStatus}</div>

        {route === "feed" && (
          <>
            <HeroStatBand stats={stats} />
            <div className="section">
              <h2 className="section-header">Live feed</h2>
              <FeedTable rows={rows} />
            </div>
          </>
        )}

        {route === "leaderboard" && (
          <>
            <h1 style={{ fontFamily: "var(--font-serif)", fontSize: 32, marginBottom: 24 }}>
              Top attackers
            </h1>
            <LeaderboardTable rows={rows} />
          </>
        )}

        {route === "methodology" && <Methodology />}
      </main>
    </div>
  );
}

function initialRoute(): Route {
  const h = window.location.hash.replace(/^#/, "");
  if (h === "leaderboard") return "leaderboard";
  if (h === "methodology") return "methodology";
  return "feed";
}

function formatHeroBadge(usd: number): string {
  if (usd >= 1_000_000) return `$${(usd / 1_000_000).toFixed(1)}M`;
  if (usd >= 1_000) return `$${(usd / 1_000).toFixed(1)}K`;
  return `$${usd.toFixed(0)}`;
}
