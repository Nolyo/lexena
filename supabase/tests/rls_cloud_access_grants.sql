BEGIN;
SELECT plan(20);

SELECT has_table('public', 'cloud_access_grants', 'cloud grants exist');
SELECT ok((SELECT relrowsecurity FROM pg_class WHERE oid = 'public.cloud_access_grants'::regclass), 'RLS enabled');

INSERT INTO auth.users (id, email) VALUES
  ('a11ce000-0000-4000-8000-000000000001', 'cloud-owner-access@test.invalid'),
  ('b0b00000-0000-4000-8000-000000000002', 'cloud-no-access@test.invalid');
INSERT INTO public.cloud_access_grants (user_id, reason)
VALUES ('a11ce000-0000-4000-8000-000000000001', 'Owner test');

SET LOCAL ROLE authenticated;
SET LOCAL "request.jwt.claims" = '{"sub":"a11ce000-0000-4000-8000-000000000001","role":"authenticated"}';
SELECT is((SELECT count(*)::int FROM public.cloud_access_grants), 1, 'owner sees own grant');
SELECT throws_ok($$UPDATE public.cloud_access_grants SET monthly_minutes_limit = 100000$$, '42501', NULL, 'owner cannot increase allowance');
SELECT throws_ok($$DELETE FROM public.cloud_access_grants$$, '42501', NULL, 'owner cannot delete grants');
SELECT throws_ok($$TRUNCATE public.cloud_access_grants$$, '42501', NULL, 'owner cannot truncate grants');

SET LOCAL "request.jwt.claims" = '{"sub":"b0b00000-0000-4000-8000-000000000002","role":"authenticated","user_metadata":{"role":"admin"}}';
SELECT is((SELECT count(*)::int FROM public.cloud_access_grants), 0, 'another user cannot read owner grants even with admin metadata');
SELECT throws_ok($$INSERT INTO public.cloud_access_grants (user_id, reason) VALUES ('b0b00000-0000-4000-8000-000000000002', 'self-grant')$$, '42501', NULL, 'cannot self-grant');
SELECT throws_ok($$UPDATE public.cloud_access_grants SET revoked_at = NULL$$, '42501', NULL, 'cannot reactivate a grant');

SET LOCAL ROLE anon;
SELECT throws_ok($$SELECT * FROM public.cloud_access_grants$$, '42501', NULL, 'anon cannot read grants');
SELECT throws_ok($$INSERT INTO public.cloud_access_grants (user_id, reason) VALUES ('b0b00000-0000-4000-8000-000000000002', 'anon')$$, '42501', NULL, 'anon cannot grant access');

SET LOCAL ROLE service_role;
SELECT is((SELECT count(*)::int FROM public.cloud_access_grants WHERE user_id = 'a11ce000-0000-4000-8000-000000000001'), 1, 'Worker can read grants');
SELECT lives_ok($$UPDATE public.cloud_access_grants SET revoked_at = NOW() WHERE user_id = 'a11ce000-0000-4000-8000-000000000001'$$, 'server can revoke grants');
SELECT throws_ok($$UPDATE public.cloud_access_grants SET monthly_minutes_limit = 0$$, '23514', NULL, 'minute limit must be positive');
SELECT throws_ok($$UPDATE public.cloud_access_grants SET monthly_tokens_limit = -1$$, '23514', NULL, 'token limit must be positive');

RESET ROLE;
INSERT INTO public.trial_credits (user_id, minutes_granted, minutes_consumed)
VALUES ('a11ce000-0000-4000-8000-000000000001', 60, 0);
INSERT INTO public.usage_events (user_id, kind, units, units_unit, model, provider, source)
VALUES ('a11ce000-0000-4000-8000-000000000001', 'transcription', 2, 'minutes', 'test', 'groq', 'complimentary');
SELECT is((SELECT units_total FROM public.usage_summary WHERE user_id = 'a11ce000-0000-4000-8000-000000000001' AND kind = 'transcription'), 2::numeric, 'complimentary usage is counted');
SELECT is((SELECT minutes_consumed FROM public.trial_credits WHERE user_id = 'a11ce000-0000-4000-8000-000000000001'), 0::numeric, 'complimentary usage does not debit trial');
SELECT is((SELECT complimentary_units_total FROM public.usage_summary WHERE user_id = 'a11ce000-0000-4000-8000-000000000001' AND kind = 'transcription'), 2::numeric, 'complimentary counter is updated atomically');
INSERT INTO public.usage_events (user_id, kind, units, units_unit, model, provider, source)
VALUES ('a11ce000-0000-4000-8000-000000000001', 'transcription', 3, 'minutes', 'test', 'groq', 'quota');
SELECT is((SELECT units_total - complimentary_units_total FROM public.usage_summary WHERE user_id = 'a11ce000-0000-4000-8000-000000000001' AND kind = 'transcription'), 3::numeric, 'paid quota excludes offered minutes');
INSERT INTO public.usage_events (user_id, kind, units, units_unit, model, provider, source)
VALUES ('a11ce000-0000-4000-8000-000000000001', 'post_process', 10, 'tokens', 'test', 'openai', 'complimentary');
SELECT is((SELECT complimentary_units_total FROM public.usage_summary WHERE user_id = 'a11ce000-0000-4000-8000-000000000001' AND kind = 'post_process'), 10::numeric, 'text usage has its own complimentary counter');

SELECT * FROM finish();
ROLLBACK;
