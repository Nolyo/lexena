# MCP local — validation de livraison

Cette checklist doit être remplie avec des résultats observés. Une case non cochée n'est pas une garantie acquise. Les fixtures utilisent des données synthétiques, jamais les notes personnelles de l'utilisateur.

## Environnement à consigner

- Version Lexena / liaison / protocole IPC : à renseigner.
- Windows, architecture et compte de test : à renseigner.
- Client MCP et version : à renseigner ; protocole MCP négocié : à renseigner.
- Distribution testée : NSIS / ZIP portable MCP.

## Installation et connexion

- [ ] Sur un Windows sans Node/Python/Rust, installer Lexena et disposer de la liaison incluse.
- [ ] MCP désactivé : aucune liaison acceptée, aucune écoute réseau ajoutée.
- [ ] Créer une connexion dans Lexena, copier sa configuration sans secret et la connecter dans un client stdio compatible.
- [ ] Lexena fermé : diagnostic clair, code de sortie non nul ; aucun accès autonome aux fichiers de notes.
- [ ] Lexena masqué dans le tray : outils disponibles.
- [ ] Révoquer la connexion pendant son utilisation : les nouveaux appels sont refusés.
- [ ] Vérifier le comportement avec deux clients simultanés et les limites de connexions/requêtes.
- [ ] Tester un chemin d'installation comportant espaces et caractères non ASCII.
- [ ] Tester une mise à jour avec une liaison ouverte : fermeture avant remplacement ou diagnostic de verrouillage explicite.

## Profils et permissions

- [ ] Un seul profil : découverte et autorisation fonctionnent sans noms imposés.
- [ ] Dix profils : tous les profils autorisés sont découvrables, aucun profil non autorisé n'est exposé.
- [ ] Profil cible absent de la demande : clarification par l'IA avant accès au contenu.
- [ ] `profile_id` absent d'un appel métier : refus avant lecture des repositories.
- [ ] Profil A affiché, appel explicite au profil B : seule la note B change, sans changement de profil UI.
- [ ] Même UUID de note dans deux profils : une sauvegarde retardée après changement de profil ne touche pas l'autre copie.
- [ ] Renommer un profil : son ID et ses droits existants restent stables.
- [ ] Ajouter un profil : aucun grant existant ne lui accorde automatiquement des droits.
- [ ] Supprimer un profil pendant un appel : aucun commit tardif ne le recrée.
- [ ] Permissions lecture seule : accès direct à un outil de mutation refusé, même s'il est absent du catalogue.
- [ ] Notes locales : invisibles sans option dédiée ; une édition autorisée conserve leur propriété locale.

## Notes, dossiers et éditeurs

- [ ] Créer, lire, rechercher, ajouter du texte, remplacer un passage unique, déplacer et mettre en favori une note.
- [ ] Titres identiques : l'IA demande de choisir la note avant modification.
- [ ] Note riche avec tableau, tâches, code, liens internes et image : aucun contenu existant perdu lors d'un ajout.
- [ ] Remplacement ciblé ambigu ou traversant plusieurs nœuds : erreur explicite.
- [ ] HTML actif ou lien local dans une entrée MCP : refus sans mutation.
- [ ] Note grande et Unicode : lecture paginée sans couper un caractère ni tronquer silencieusement le document.
- [ ] Remplacement intégral sans lecture HTML complète ou jeton valide : refus.
- [ ] Éditeur principal dirty : appel MCP refusé, texte utilisateur conservé.
- [ ] Même scénario en fenêtre détachée.
- [ ] Frappe au même instant qu'un ajout MCP : sérialisation ou conflit visible, aucun buffer écrasé.
- [ ] Éditeur propre : modification MCP reflétée dans l'éditeur et la liste sans sauvegarde de l'ancienne version.
- [ ] Supprimer un dossier : ses notes restent disponibles à la racine.
- [ ] Supprimer puis restaurer une note, y compris après purge du tombstone local.

## Interruption et synchronisation

- [ ] Rejouer la même opération après perte de réponse : un seul ajout, même reçu.
- [ ] Réutiliser un ID d'opération avec un autre contenu : refus.
- [ ] Interrompre aux frontières journal/contenu/métadonnées/outbox/reçu, redémarrer et retrouver un état complet.
- [ ] Importer une intention dans la queue, la consommer puis interrompre avant acquittement outbox : aucun réimport grâce au watermark.
- [ ] Modifier un profil inactif : sauvegarde locale immédiate, état sync différé explicite.
- [ ] Charger ce profil : reprise avant pull destructif, aucune contamination du profil précédent.
- [ ] Réseau indisponible ou DLQ : modification locale conservée et application distante différée.
- [ ] Sync désactivée : le MCP ne l'active pas.
- [ ] Note hors cap de mutation ou historique plein : refus avant modification.

## Nouveau PC et récupération

- [ ] Installer Lexena avec liaison incluse ; récupérer les profils synchronisés, puis recréer les autorisations MCP locales.
- [ ] Vérifier la configuration produite avec les nouveaux chemins et IDs locaux.
- [ ] Tester une récupération locale compatible pour les données non synchronisées ; vérifier l'historique et les chemins audio séparément.
- [ ] Ne pas considérer le backup JSON existant ni l'export GDPR comme une sauvegarde complète multi-profil validée.
- [ ] Confirmer les contenus sur le nouveau PC avant de supprimer la copie de l'ancien.

## Résultats observés

Les résultats automatisés et les limites d'environnement sont consignés dans `implementation-decisions.md` et les rapports de développement. Les scénarios natifs non exécutés restent ouverts dans cette checklist.

### Client de conformité — 5 novembre 2026

MCP Inspector officiel **2.14.0**, Node de test **24.22.1**, liaison release **3.2.2-beta.1**, IPC **1**. Exécution CLI stdio uniquement, sans proxy ou écoute réseau, sur le vrai serveur MCP Rust dans une fixture isolée. Données, configuration et secret du coffre synthétiques, supprimés après l'essai ; aucune note personnelle utilisée.

Résultats observés : négociation **2026-07-28** et **2025-11-25**, catalogue de **17 outils**, contexte limité au profil autorisé, création UUID, ajout avec révision, répétition identique depuis une nouvelle session, lecture HTML et vérification Unicode des fichiers persistés. Huit commandes réussies ; un appel sans profil a produit le rejet attendu. Inspector masque le détail `PROFILE_REQUIRED`, couvert séparément par les tests SDK.

Validation stricte des schémas : **0 erreur**, **47 avertissements de portabilité** liés aux unions de types nullable. L'essai initial a pris 26,254 secondes. Une nouvelle exécution sur l'exécutable de tests de la bibliothèque Tauri finale a réussi en **20,595 secondes**, avec les mêmes résultats, nettoyage du coffre synthétique et terminaison des processus vérifiés. Cette validation de la liaison et du serveur ne remplace pas une installation NSIS ou une session d'éditeur natif.

### Couverture automatisée constatée

- Suite frontend complète sur les sources finales : **711 tests réussis dans 94 fichiers**, avec `pnpm test -- --maxWorkers=2 --testTimeout=30000`. Le build frontend inclut également la vérification TypeScript.
- Bibliothèque Tauri réelle sur les sources finales : **237 tests réussis, aucun échec**, avec `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --lib --locked --jobs 2 -- --test-threads=2`. L'unique fixture enfant marquée `ignored` est exécutée explicitement par son test parent ; elle n'est pas un scénario d'acceptation laissé de côté.
- Serveur MCP : **24 tests ciblés réussis** après relecture ; suite de services isolée précédente **121 tests réussis**, IPC **17 tests réussis**, répétitions de deux clients release simultanés réussies.
- Reprise sync/queue : **194 tests**, puis **52 tests ciblés** pour le rejet des anciennes reprises DLQ ; tests Rust outbox et snapshot atomique réussis.
- Scripts de build, manifeste portable et scan des binaires : **7 tests réussis**, dont création d'un ZIP depuis un autre répertoire courant et détection de secrets au-delà de 50 Mio.

La relecture globale puis la relecture des correctifs ont approuvé les protections de réinitialisation, le drainage des requêtes et audits, ainsi que l'ordre de rechargement des métadonnées et révisions des éditeurs. Les suites complètes ci-dessus ont été exécutées après ces correctifs. Les scénarios d'installation et d'interface native restent à effectuer.

### Build natif local

Le build CPU par la CLI Tauri a compilé l'application release et produit un NSIS le 5 novembre. La commande standard a ensuite échoué lors de la génération de la signature d'updater, car `TAURI_SIGNING_PRIVATE_KEY` n'était pas fournie. La clé privée de signature n'a pas été chargée ; aucune identité de signature n'a été créée ou modifiée pour contourner cette étape.

La validation locale utilise donc une surcharge temporaire `bundle.createUpdaterArtifacts: false`, sans modifier la configuration livrée. Ce paquet d'essai ne constitue pas une publication signée. La chaîne de release existante conserve sa vérification cryptographique et son identité de signature.

Le build final CPU avec cette surcharge a terminé avec **code de sortie 0**, via la CLI Tauri ; vérification TypeScript, bundling frontend, compilation native release et génération NSIS ont réussi. Le script NSIS généré contient l'installation et la suppression de `lexena-mcp.exe` à côté de l'application. Aucune installation n'a été lancée sur le profil utilisateur.

Le ZIP portable local a ensuite été généré et scanné sans détection de secret. Vérification après décompression en mémoire : exactement `lexena.exe`, `lexena-mcp.exe` et `mcp-version.json` ; versions application/liaison **3.2.2-beta.1**, IPC **1**, target **x86_64-pc-windows-msvc** ; empreintes SHA-256 des deux exécutables conformes au manifeste.

Artefacts locaux sous `src-tauri/target/release/bundle/` :

- NSIS : `nsis/Lexena_3.2.2-beta.1_x64-setup.exe`, SHA-256 `16866d861a9f8d843723a6e572cb0871d9442741dfc3ee87b9f90fa898e8fa65`.
- ZIP d'essai : `portable/Lexena_3.2.2-beta.1_x64-portable-mcp-local-test.zip`, SHA-256 `7c5aeb1fc6ba93a1f769e01089c62a399f7d9863dd79afa7b93a8aa1b244e275`.

Le bundler a également émis l'avertissement `__TAURI_BUNDLE_TYPE variable not found` avec les versions Tauri/CLI du dépôt ; le cycle d'update installé reste à vérifier. Ni cet avertissement ni la présence des deux EXE ne sont comptés comme une validation d'installation ou de mise à jour.

### Mesure de performance — 5 novembre 2026

Service MCP de production compilé en release, appels `invoke` incluant autorisation et stockage (hors transport stdio et UI), Rust 1.93.1, Windows 10.0.26300 x86_64, AMD Ryzen 5 2600X ; répertoire temporaire Windows, données synthétiques. Corpus : 5 000 notes, 2 275 octets HTML par note (11 375 000 octets au total), révisions persistées. Dix mesures chaudes par outil, première page de 20 notes, recherche `ÉTÉ` insensible à la casse et normalisée Unicode. Des builds Rust étaient également en cours sur le poste.

| Outil | p50 | p95 | Cible p95 hors construction initiale |
| --- | ---: | ---: | ---: |
| Liste, première page | 36,73 ms | 50,43 ms | < 300 ms |
| Recherche lexicale | 37,47 ms | 55,70 ms | < 1 500 ms |

La première construction a renvoyé `TIMEOUT` après 30 028 ms. Le job suivi a été laissé se terminer avant la mesure chaude ; il n'a pas été abandonné. Durée du scénario complet, création du corpus comprise : 131 secondes. Ces mesures valident ce corpus et ce poste ; elles ne remplacent pas les essais d'installation ou d'éditeur avec une session native.
