use super::{cache::CachePolicy, headers::CanonicalUrl, MatchSemver};
use crate::{
    docbuilder::Limits,
    impl_axum_webpage,
    web::{error::AxumResult, extractors::DbConnection, match_version, MetaData},
    Config,
};
use anyhow::Result;
use axum::{
    extract::{Extension, Path},
    http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Build {
    id: i32,
    rustc_version: String,
    docsrs_version: String,
    build_status: bool,
    build_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct BuildsPage {
    metadata: MetaData,
    builds: Vec<Build>,
    limits: Limits,
    canonical_url: CanonicalUrl,
    use_direct_platform_links: bool,
}

impl_axum_webpage! {
    BuildsPage = "crate/builds.html",
}

pub(crate) async fn build_list_handler(
    Path((name, req_version)): Path<(String, String)>,
    mut conn: DbConnection,
    Extension(config): Extension<Arc<Config>>,
) -> AxumResult<impl IntoResponse> {
    let (version, version_or_latest) = match match_version(&mut conn, &name, Some(&req_version))
        .await?
        .exact_name_only()?
    {
        MatchSemver::Exact((version, _)) => (version.clone(), version),
        MatchSemver::Latest((version, _)) => (version, "latest".to_string()),

        MatchSemver::Semver((version, _)) => {
            return Ok(super::axum_cached_redirect(
                &format!("/crate/{name}/{version}/builds"),
                CachePolicy::ForeverInCdn,
            )?
            .into_response());
        }
    };

    Ok(BuildsPage {
        metadata: MetaData::from_crate(&mut conn, &name, &version, &version_or_latest).await?,
        builds: get_builds(&mut conn, &name, &version).await?,
        limits: Limits::for_crate(&config, &mut conn, &name).await?,
        canonical_url: CanonicalUrl::from_path(format!("/crate/{name}/latest/builds")),
        use_direct_platform_links: true,
    }
    .into_response())
}

pub(crate) async fn build_list_json_handler(
    Path((name, req_version)): Path<(String, String)>,
    mut conn: DbConnection,
) -> AxumResult<impl IntoResponse> {
    let version = match match_version(&mut conn, &name, Some(&req_version))
        .await?
        .exact_name_only()?
    {
        MatchSemver::Exact((version, _)) | MatchSemver::Latest((version, _)) => version,
        MatchSemver::Semver((version, _)) => {
            return Ok(super::axum_cached_redirect(
                &format!("/crate/{name}/{version}/builds.json"),
                CachePolicy::ForeverInCdn,
            )?
            .into_response());
        }
    };

    Ok((
        Extension(CachePolicy::NoStoreMustRevalidate),
        [(ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(get_builds(&mut conn, &name, &version).await?),
    )
        .into_response())
}

async fn get_builds(
    conn: &mut sqlx::PgConnection,
    name: &str,
    version: &str,
) -> Result<Vec<Build>> {
    Ok(sqlx::query_as!(
        Build,
        "SELECT
            builds.id,
            builds.rustc_version,
            builds.docsrs_version,
            builds.build_status,
            builds.build_time
         FROM builds
         INNER JOIN releases ON releases.id = builds.rid
         INNER JOIN crates ON releases.crate_id = crates.id
         WHERE crates.name = $1 AND releases.version = $2
         ORDER BY id DESC",
        name,
        version,
    )
    .fetch_all(&mut *conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use crate::{
        test::{assert_cache_control, wrapper, FakeBuild},
        web::cache::CachePolicy,
    };
    use chrono::{DateTime, Duration, Utc};
    use kuchikiki::traits::TendrilSink;
    use reqwest::StatusCode;

    #[test]
    fn build_list() {
        wrapper(|env| {
            env.fake_release()
                .name("foo")
                .version("0.1.0")
                .builds(vec![
                    FakeBuild::default()
                        .rustc_version("rustc (blabla 2019-01-01)")
                        .docsrs_version("docs.rs 1.0.0"),
                    FakeBuild::default()
                        .successful(false)
                        .rustc_version("rustc (blabla 2020-01-01)")
                        .docsrs_version("docs.rs 2.0.0"),
                    FakeBuild::default()
                        .rustc_version("rustc (blabla 2021-01-01)")
                        .docsrs_version("docs.rs 3.0.0"),
                ])
                .create()?;

            let response = env.frontend().get("/crate/foo/0.1.0/builds").send()?;
            assert_cache_control(&response, CachePolicy::NoCaching, &env.config());
            let page = kuchikiki::parse_html().one(response.text()?);

            let rows: Vec<_> = page
                .select("ul > li a.release")
                .unwrap()
                .map(|row| row.text_contents())
                .collect();

            assert!(rows[0].contains("rustc (blabla 2021-01-01)"));
            assert!(rows[0].contains("docs.rs 3.0.0"));
            assert!(rows[1].contains("rustc (blabla 2020-01-01)"));
            assert!(rows[1].contains("docs.rs 2.0.0"));
            assert!(rows[2].contains("rustc (blabla 2019-01-01)"));
            assert!(rows[2].contains("docs.rs 1.0.0"));

            Ok(())
        });
    }

    #[test]
    fn build_list_json() {
        wrapper(|env| {
            env.fake_release()
                .name("foo")
                .version("0.1.0")
                .builds(vec![
                    FakeBuild::default()
                        .rustc_version("rustc (blabla 2019-01-01)")
                        .docsrs_version("docs.rs 1.0.0"),
                    FakeBuild::default()
                        .successful(false)
                        .rustc_version("rustc (blabla 2020-01-01)")
                        .docsrs_version("docs.rs 2.0.0"),
                    FakeBuild::default()
                        .rustc_version("rustc (blabla 2021-01-01)")
                        .docsrs_version("docs.rs 3.0.0"),
                ])
                .create()?;

            let response = env.frontend().get("/crate/foo/0.1.0/builds.json").send()?;
            assert_cache_control(&response, CachePolicy::NoStoreMustRevalidate, &env.config());
            let value: serde_json::Value = serde_json::from_str(&response.text()?)?;

            assert_eq!(value.pointer("/0/build_status"), Some(&true.into()));
            assert_eq!(
                value.pointer("/0/docsrs_version"),
                Some(&"docs.rs 3.0.0".into())
            );
            assert_eq!(
                value.pointer("/0/rustc_version"),
                Some(&"rustc (blabla 2021-01-01)".into())
            );
            assert!(value.pointer("/0/id").unwrap().is_i64());
            assert!(serde_json::from_value::<DateTime<Utc>>(
                value.pointer("/0/build_time").unwrap().clone()
            )
            .is_ok());

            assert_eq!(value.pointer("/1/build_status"), Some(&false.into()));
            assert_eq!(
                value.pointer("/1/docsrs_version"),
                Some(&"docs.rs 2.0.0".into())
            );
            assert_eq!(
                value.pointer("/1/rustc_version"),
                Some(&"rustc (blabla 2020-01-01)".into())
            );
            assert!(value.pointer("/1/id").unwrap().is_i64());
            assert!(serde_json::from_value::<DateTime<Utc>>(
                value.pointer("/1/build_time").unwrap().clone()
            )
            .is_ok());

            assert_eq!(value.pointer("/2/build_status"), Some(&true.into()));
            assert_eq!(
                value.pointer("/2/docsrs_version"),
                Some(&"docs.rs 1.0.0".into())
            );
            assert_eq!(
                value.pointer("/2/rustc_version"),
                Some(&"rustc (blabla 2019-01-01)".into())
            );
            assert!(value.pointer("/2/id").unwrap().is_i64());
            assert!(serde_json::from_value::<DateTime<Utc>>(
                value.pointer("/2/build_time").unwrap().clone()
            )
            .is_ok());

            assert!(
                value.pointer("/1/build_time").unwrap().as_str().unwrap()
                    < value.pointer("/0/build_time").unwrap().as_str().unwrap()
            );
            assert!(
                value.pointer("/2/build_time").unwrap().as_str().unwrap()
                    < value.pointer("/1/build_time").unwrap().as_str().unwrap()
            );

            Ok(())
        });
    }

    #[test]
    fn limits() {
        wrapper(|env| {
            env.fake_release().name("foo").version("0.1.0").create()?;

            env.db().conn().query(
                "INSERT INTO sandbox_overrides
                    (crate_name, max_memory_bytes, timeout_seconds, max_targets)
                 VALUES ($1, $2, $3, $4)",
                &[
                    &"foo",
                    &(6 * 1024 * 1024 * 1024i64),
                    &(Duration::hours(2).num_seconds() as i32),
                    &1,
                ],
            )?;

            let page = kuchikiki::parse_html().one(
                env.frontend()
                    .get("/crate/foo/0.1.0/builds")
                    .send()?
                    .text()?,
            );

            let header = page.select(".about h4").unwrap().next().unwrap();
            assert_eq!(header.text_contents(), "foo's sandbox limits");

            let values: Vec<_> = page
                .select(".about table tr td:last-child")
                .unwrap()
                .map(|row| row.text_contents())
                .collect();
            let values: Vec<_> = values.iter().map(|v| &**v).collect();

            dbg!(&values);
            assert!(values.contains(&"6 GB"));
            assert!(values.contains(&"2 hours"));
            assert!(values.contains(&"100 kB"));
            assert!(values.contains(&"blocked"));
            assert!(values.contains(&"1"));

            Ok(())
        });
    }

    #[test]
    fn latest_200() {
        wrapper(|env| {
            env.fake_release()
                .name("aquarelle")
                .version("0.1.0")
                .builds(vec![FakeBuild::default()
                    .rustc_version("rustc (blabla 2019-01-01)")
                    .docsrs_version("docs.rs 1.0.0")])
                .create()?;

            env.fake_release()
                .name("aquarelle")
                .version("0.2.0")
                .builds(vec![FakeBuild::default()
                    .rustc_version("rustc (blabla 2019-01-01)")
                    .docsrs_version("docs.rs 1.0.0")])
                .create()?;

            let resp = env
                .frontend()
                .get("/crate/aquarelle/latest/builds")
                .send()?;
            assert!(resp
                .url()
                .as_str()
                .ends_with("/crate/aquarelle/latest/builds"));
            let body = String::from_utf8(resp.bytes().unwrap().to_vec()).unwrap();
            assert!(body.contains("<a href=\"/crate/aquarelle/latest/features\""));
            assert!(body.contains("<a href=\"/crate/aquarelle/latest/builds\""));
            assert!(body.contains("<a href=\"/crate/aquarelle/latest/source/\""));
            assert!(body.contains("<a href=\"/crate/aquarelle/latest\""));

            let resp_json = env
                .frontend()
                .get("/crate/aquarelle/latest/builds.json")
                .send()?;
            assert!(resp_json
                .url()
                .as_str()
                .ends_with("/crate/aquarelle/latest/builds.json"));

            Ok(())
        });
    }

    #[test]
    fn crate_version_not_found() {
        wrapper(|env| {
            env.fake_release()
                .name("foo")
                .version("0.1.0")
                .builds(vec![FakeBuild::default()
                    .rustc_version("rustc (blabla 2019-01-01)")
                    .docsrs_version("docs.rs 1.0.0")])
                .create()?;

            let resp = env.frontend().get("/crate/foo/0.2.0/builds").send()?;
            dbg!(resp.url().as_str());
            assert!(resp.url().as_str().ends_with("/crate/foo/0.2.0/builds"));
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
    }

    #[test]
    fn invalid_semver() {
        wrapper(|env| {
            env.fake_release()
                .name("foo")
                .version("0.1.0")
                .builds(vec![FakeBuild::default()
                    .rustc_version("rustc (blabla 2019-01-01)")
                    .docsrs_version("docs.rs 1.0.0")])
                .create()?;

            let resp = env.frontend().get("/crate/foo/0,1,0/builds").send()?;
            dbg!(resp.url().as_str());
            assert!(resp.url().as_str().ends_with("/crate/foo/0,1,0/builds"));
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            Ok(())
        });
    }
}
