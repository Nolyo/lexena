-- Explicit, revocable cloud access independent of Lemon Squeezy subscriptions.
-- No account is granted access by this migration. Provision from the SQL editor.
CREATE TABLE public.cloud_access_grants (
  user_id UUID PRIMARY KEY REFERENCES auth.users(id) ON DELETE CASCADE,
  monthly_minutes_limit INTEGER NOT NULL DEFAULT 1000
    CHECK (monthly_minutes_limit BETWEEN 1 AND 100000),
  monthly_tokens_limit INTEGER NOT NULL DEFAULT 1000000
    CHECK (monthly_tokens_limit BETWEEN 1 AND 100000000),
  expires_at TIMESTAMPTZ,
  revoked_at TIMESTAMPTZ,
  reason TEXT NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 500),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE public.cloud_access_grants ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.cloud_access_grants FROM PUBLIC, anon, authenticated;
GRANT SELECT ON public.cloud_access_grants TO authenticated;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.cloud_access_grants TO service_role;

CREATE POLICY cloud_access_grants_owner_read
  ON public.cloud_access_grants FOR SELECT TO authenticated
  USING ((SELECT auth.uid()) = user_id);

COMMENT ON TABLE public.cloud_access_grants IS
  'Server-managed complimentary cloud access, not an admin role. Clients can only read their own grant. Revocation is checked on each Worker request. Monthly limits use UTC usage_summary totals.';

-- Separate complimentary usage from both paid quota and billable overage.
ALTER TABLE public.usage_events DROP CONSTRAINT usage_events_source_check;
ALTER TABLE public.usage_events ADD CONSTRAINT usage_events_source_check
  CHECK (source IN ('trial', 'quota', 'overage', 'complimentary'));

-- Keep the existing all-source total for owner limits, but never convert
-- complimentary minutes into paid quota/overage after revocation or expiry.
ALTER TABLE public.usage_summary
  ADD COLUMN complimentary_units_total NUMERIC(12, 4) NOT NULL DEFAULT 0
  CHECK (complimentary_units_total >= 0 AND complimentary_units_total <= units_total);

CREATE SCHEMA IF NOT EXISTS cloud_access_private;
REVOKE ALL ON SCHEMA cloud_access_private FROM PUBLIC, anon, authenticated;

CREATE FUNCTION cloud_access_private.track_complimentary_usage()
RETURNS TRIGGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
BEGIN
  UPDATE public.usage_summary
  SET complimentary_units_total = complimentary_units_total + NEW.units
  WHERE user_id = NEW.user_id
    AND year_month = to_char(NEW.created_at AT TIME ZONE 'UTC', 'YYYY-MM')
    AND kind = NEW.kind;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'usage summary missing for complimentary event';
  END IF;
  RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION cloud_access_private.track_complimentary_usage() FROM PUBLIC, anon, authenticated;

-- PostgreSQL fires same-event triggers alphabetically: aggregate first, then
-- complimentary. Both counters are updated in the event insert transaction.
CREATE TRIGGER trg_usage_events_complimentary
  AFTER INSERT ON public.usage_events
  FOR EACH ROW WHEN (NEW.source = 'complimentary')
  EXECUTE FUNCTION cloud_access_private.track_complimentary_usage();
