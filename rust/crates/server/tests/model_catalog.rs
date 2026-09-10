use axum::Router;
use server::{app, AppState};

#[tokio::test]
async fn web_models_endpoint_matches_shared_catalog() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener
        .local_addr()
        .expect("test listener should expose an address");
    let router: Router = app(AppState::new());

    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test server should run");
    });

    let response = reqwest::get(format!("http://{address}/models"))
        .await
        .expect("models endpoint should respond")
        .error_for_status()
        .expect("models endpoint should return success")
        .json::<server::ListModelsResponse>()
        .await
        .expect("models endpoint should return valid JSON");

    server.abort();

    assert_eq!(response.models.len(), api::MODEL_CATALOG.len() + 1);
    for entry in api::MODEL_CATALOG {
        let model = response
            .models
            .iter()
            .find(|model| model.alias == entry.alias)
            .unwrap_or_else(|| panic!("web model catalog is missing alias {}", entry.alias));
        assert_eq!(model.model, entry.model, "stale model for {}", entry.alias);
        assert_eq!(model.label, entry.label, "stale label for {}", entry.alias);
    }
}
