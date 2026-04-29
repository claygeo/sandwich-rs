import { createClient } from "@supabase/supabase-js";

const url = import.meta.env.VITE_SUPABASE_URL;
const anonKey = import.meta.env.VITE_SUPABASE_ANON_KEY;

if (!url || !anonKey) {
  console.warn(
    "[supabase] VITE_SUPABASE_URL or VITE_SUPABASE_ANON_KEY missing — frontend will run in mock mode"
  );
}

export const supabase = createClient(
  url || "https://placeholder.supabase.co",
  anonKey || "placeholder",
  {
    realtime: {
      params: {
        eventsPerSecond: 20,
      },
    },
  }
);

export const isSupabaseConfigured = Boolean(url) && Boolean(anonKey);
