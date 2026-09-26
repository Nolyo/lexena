# Accès cloud propriétaire

Un droit explicite dans `public.cloud_access_grants` permet d'utiliser la
transcription, le post-traitement et l'assistant des notes sans abonnement Lemon
Squeezy. Il ne confère aucun droit d'administration ni d'accès aux autres comptes.

## Déploiement

Préconditions : connexion aux consoles Supabase et Cloudflare, compte propriétaire
vérifié, tests SQL et Worker réussis. Aucun secret supplémentaire n'est nécessaire.

1. Appliquer `supabase/migrations/20260926110447_owner_cloud_access.sql` via
   `pnpm exec supabase db push` après vérification de `--dry-run`.
2. Depuis `workers/transcription-api`, déployer le Worker avec
   `pnpm exec wrangler deploy --env production`. La migration doit précéder le
   Worker ; les nouveaux contrôles refusent les requêtes si la lecture des droits
   échoue. Les quatre secrets existants restent nécessaires.
3. Installer une version du client contenant ce changement. Pour le développement,
   l'utilisateur peut démarrer `pnpm tauri dev` ; pour un installateur, utiliser
   `pnpm tauri build`.
4. Attribuer le droit ci-dessous au compte vérifié. Dans Lexena : Paramètres →
   Cloud → Rafraîchir, puis choisir Lexena Cloud dans Transcription si nécessaire.

## Attribution et révocation

Exécuter depuis le SQL Editor Supabase avec un compte d'administration. Remplacer
les deux valeurs par l'UUID **et** l'adresse vérifiés dans Authentication → Users.
Aucun compte n'est ajouté par la migration et aucun identifiant de propriétaire
n'est codé dans le logiciel.

```sql
insert into public.cloud_access_grants
  (user_id, reason, monthly_minutes_limit, monthly_tokens_limit)
select id, 'Owner cloud access', 1000, 1000000
from auth.users
where id = '<OWNER_UUID>'::uuid
  and lower(email) = lower('<OWNER_EMAIL>')
  and email_confirmed_at is not null
returning user_id, monthly_minutes_limit, monthly_tokens_limit, expires_at;
```

L'absence de ligne retournée indique une identité incorrecte/non vérifiée.
Une attribution existante provoque une erreur : vérifier avant de la réactiver.
`expires_at = null` signifie sans échéance. Les limites sont modifiables uniquement
depuis le serveur. Pour révoquer immédiatement les nouvelles requêtes :

```sql
update public.cloud_access_grants
set revoked_at = now()
where user_id = '<OWNER_UUID>'::uuid
returning user_id, revoked_at;
```

Le client actualise son affichage au retour au premier plan et toutes les minutes.
Le Worker relit le droit à chaque requête ; les opérations déjà autorisées/en
cours peuvent se terminer. Après révocation, un essai ou abonnement valide reste
utilisable normalement. L'usage offert antérieur ne réduit pas le quota payant.

## Sécurité et consommation

- L'identité provient du JWT Supabase signé et vérifié par le Worker. Ni une
  adresse e-mail dans une requête, ni `user_metadata`, ni un réglage local ne
  donnent de droit. Activer la double authentification sur le compte propriétaire
  et les consoles d'administration protège aussi contre le vol du compte.
- RLS limite la lecture au compte concerné. Les rôles `anon` et `authenticated`
  n'ont aucun droit d'écriture, suppression ou troncature sur les attributions.
  Il n'existe aucun endpoint d'auto-attribution. Les clés privilégiées restent
  côté serveur.
- La consommation est journalisée avec `source = 'complimentary'`, sans audio ni
  texte. Le compteur mensuel UTC conserve le total et un sous-total offert,
  maintenus dans la même transaction. Aucun dépassement payant n'est sélectionné
  pour un droit propriétaire actif.
- Les plafonds par défaut sont 1 000 minutes et 1 000 000 tokens par mois,
  paramétrables en base. Ils portent sur les totaux du compte pour le mois UTC,
  y compris l'usage précédant l'attribution. Comme les quotas existants, ce sont
  des contrôles **avant requête**, sans réservation concurrente : une requête ou
  des requêtes déjà en vol peuvent dépasser le solde restant. Ils ne constituent
  pas un budget fournisseur strict ; utiliser aussi les limites de dépense des
  fournisseurs pour cela.
- Les quotas de synchronisation des notes conservent leurs règles existantes.

## Vérification et diagnostic

Les tests `supabase/tests/rls_cloud_access_grants.sql` vérifient l'isolation,
l'impossibilité de s'accorder/réactiver un droit et les compteurs. Les tests Worker
couvrent révocation, expiration, panne DB, plafonds, usage offert et retour au
payant. Les tests client couvrent le routage et les réponses d'un ancien compte.

`GET https://api.lexena.app/health` vérifie uniquement le Worker. Une réponse 200
ne prouve pas l'état de Supabase ou des fournisseurs. Vérifier ensuite le statut
Supabase, l'attribution, puis une courte transcription et un post-traitement depuis
l'application connectée. En cas d'échec, inspecter les logs du Worker et les
secrets `SUPABASE_URL`, `SUPABASE_SECRET_KEY`, `GROQ_API_KEY`, `OPENAI_API_KEY` sans
les copier dans les logs ou dans le client.

Références : [RLS Supabase](https://supabase.com/docs/guides/database/postgres/row-level-security),
[logs Cloudflare](https://developers.cloudflare.com/workers/observability/logs/workers-logs/).

## Retour arrière

Pour supprimer uniquement cet accès, révoquer la ligne comme décrit ci-dessus.
Pour revenir au Worker précédent, utiliser le rollback Cloudflare et réinstaller
le client précédent. Conserver la migration et les événements offerts : supprimer
la colonne ou la contrainte rendrait l'historique incompatible. Le Worker précédent
ne sait pas exclure les minutes offertes du quota payant ; éviter un retour au
payant sur ce Worker durant le même mois, ou redéployer la version corrigée.

## Historique d'exécution

- 2026-09-26 : 20 contrôles pgTAP validés dans une transaction annulée ; migration
  appliquée au projet lié ; Worker production déployé, version
  `bfabb510-1bed-4c4e-ba28-2c3c2320a1a6` (précédente :
  `9327cea3-50fe-42ee-8313-942574ef46e8`). Droit attribué au compte propriétaire
  après contrôle UUID + e-mail vérifié. Contrôles après déploiement : droit actif,
  `/health` 200 et requête sans authentification refusée en 401. Compilation
  frontend, tests client et 63 tests Worker validés. Fonctionnement du nouveau
  client confirmé par l'utilisateur après déploiement.
