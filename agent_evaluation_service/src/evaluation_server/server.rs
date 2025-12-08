use axum::{
    Json,
    Router,
    extract::{State},
    routing::{get, post},
    http::StatusCode,
};
use tracing::{info,trace};
use std::sync::Arc;
use chrono::Utc;
use agent_models::evaluation::evaluation_models::{AgentEvaluationLogData,JudgeEvaluation,EvaluatedAgentData};
use crate::evaluation_server::judge_agent::JudgeAgent;
use configuration::AgentConfig;
use redb::{Database, TableDefinition, ReadableTable, ReadableDatabase};


const EVALUATIONS_TABLE: TableDefinition<&str, Vec<u8>> = TableDefinition::new("evaluations");

/// Application state holding evaluation data.
#[derive(Clone)]
pub struct AppState {
    pub judge_agent: Arc<JudgeAgent>,
    pub db: Arc<Database>,
}

/// EvaluationServer for handling agent evaluation logs.
pub struct EvaluationServer {
    pub uri: String,
    pub app:Router,
}

impl EvaluationServer {
    pub async fn new(uri:String, agent_config: AgentConfig,  agent_api_key:String) -> anyhow::Result<Self> {
        let judge_agent=JudgeAgent::new(agent_config.clone(),agent_api_key).await?;

        // Initialize redb
        let db = Arc::new(Database::create("evaluation_db.redb")?);
        {
            let write_txn = db.begin_write()?;
            {
                let _ = write_txn.open_table(EVALUATIONS_TABLE)?;
            }
            write_txn.commit()?;
        }
        
        // Create AppState
        let app_state = AppState {
            judge_agent: Arc::new(judge_agent),
            db: db.clone(),
        };

        let app = Router::new()
            .route("/", get(root))
            .route("/log", post(log_evaluation))
            .route("/evaluations", get(list_evaluations))
            .with_state(app_state);

        Ok(Self {
            uri:uri,
            app,
        })
    }

    /// Start the HTTP server.
    pub async fn start_http(&self) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.uri.clone()).await?;
        println!("Evaluation Server started at {}", self.uri);
        axum::serve(listener, self.app.clone()).await?;
        Ok(())
    }
}

async fn root() -> &'static str {
    "Hello, Swarm Evaluation Service!"
}

async fn log_evaluation(
    State(state): State<AppState>,
    Json(log_data): Json<AgentEvaluationLogData>,
) -> Result<Json<JudgeEvaluation>, (StatusCode, String)> {
    
    let judge_agent = state.judge_agent.to_owned();
    let db = state.db.to_owned();

    info!("Received log_evaluation request for agent: {}", log_data.agent_id);

    match judge_agent.evaluate_agent_output(log_data.clone()).await {
        Ok(judge_evaluation) => {
            trace!("Received Agent Evaluation Data : {:?}", judge_evaluation);
            let evaluated_data = EvaluatedAgentData {
                agent_log: log_data.clone(),
                evaluation: judge_evaluation.clone(),
                timestamp: Utc::now().to_rfc3339(),
            };

            let write_txn = match db.begin_write() {
                Ok(txn) => txn,
                Err(e) => {
                    let error_message = format!("Failed to begin write transaction: {:?}", e);
                    trace!("{}", error_message);
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
                }
            };
            {
                let mut table: redb::Table<'_, &str, Vec<u8>> = match write_txn.open_table(EVALUATIONS_TABLE) {
                    Ok(tbl) => tbl,
                    Err(e) => {
                        let error_message = format!("Failed to open table: {:?}", e);
                        trace!("{}", error_message);
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
                    }
                };
                match table.insert(log_data.request_id.as_str(), serde_json::to_vec(&evaluated_data).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?) {
                    Ok(_) => {},
                    Err(e) => {
                        let error_message = format!("Failed to insert data: {:?}", e);
                        trace!("{}", error_message);
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
                    }
                };
            }
            match write_txn.commit() {
                Ok(_) => {},
                Err(e) => {
                    let error_message = format!("Failed to commit transaction: {:?}", e);
                    trace!("{}", error_message);
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
                }
            };

            Ok(Json(judge_evaluation))
        }
        Err(e) => {
            let error_message = format!("Failed to evaluate agent output: {:?}", e);
            trace!("{}", error_message);
            Err((StatusCode::INTERNAL_SERVER_ERROR, error_message))
        }
    }
}

async fn list_evaluations(
    State(state): State<AppState>,
) -> Result<Json<Vec<EvaluatedAgentData>>, (StatusCode, String)> {
    let db = state.db.to_owned();
    let mut evaluation_list = Vec::new();

    let read_txn = match db.begin_read() {
        Ok(txn) => txn,
        Err(e) => {
            let error_message = format!("Failed to begin read transaction: {:?}", e);
            trace!("{}", error_message);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
        }
    };

    let table: redb::ReadOnlyTable< &str, Vec<u8>> = match read_txn.open_table(EVALUATIONS_TABLE) {
        Ok(tbl) => tbl,
        Err(e) => {
            let error_message = format!("Failed to open table: {:?}", e);
            trace!("{}", error_message);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
        }
    };

    let table_iter = match table.iter() {
        Ok(iter) => iter,
        Err(e) => {
            let error_message = format!("Failed to get iterator from table: {:?}", e);
            trace!("{}", error_message);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
        }
    };

    for item_result in table_iter {
        let (_key_ref, value_ref) = match item_result {
            Ok(pair) => pair,
            Err(e) => {
                let error_message = format!("Failed to retrieve item from table: {:?}", e);
                trace!("{}", error_message);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, error_message));
            }
        };

        // Convert key_ref and value_ref to owned types if necessary for `EvaluatedAgentData`
        let evaluated_data: EvaluatedAgentData = serde_json::from_slice(&value_ref.value().to_vec()).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        evaluation_list.push(evaluated_data);
    }

    Ok(Json(evaluation_list))
}