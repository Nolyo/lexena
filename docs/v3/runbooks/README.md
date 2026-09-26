# Runbooks v3

Procédures opérationnelles pour la v3 (rotation secrets, backup, incident).

## Index

- [`secrets-rotation.md`](secrets-rotation.md) — rotation JWT secret, webhook LS, service role key, clé updater
- [`backup-restore-test.md`](backup-restore-test.md) — test trimestriel de restauration Supabase
- [`incident-response.md`](incident-response.md) — plan incident + notification GDPR <72h
- [`managed-transcription.md`](managed-transcription.md) — Worker Cloudflare api.lexena.app, providers Groq/OpenAI, monitoring + incidents
- [`owner-cloud-access.md`](owner-cloud-access.md) — attribution, révocation et vérification de l'accès cloud propriétaire

## Convention

- Chaque runbook suit le format : **Préconditions** → **Procédure pas-à-pas** → **Vérification** → **Rollback**.
- Toute exécution d'un runbook est datée dans la section "Historique d'exécution" du runbook concerné.
- Toute modification de runbook passe par une PR.
