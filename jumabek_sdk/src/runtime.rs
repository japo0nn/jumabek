use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{
    SkillError, SkillModule,
    protocol::{ExecuteParams, SkillRequest, SkillResponse, SkillResponsePayload},
};

pub async fn run_skill<S: SkillModule>(skill: S) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<SkillRequest>(&line) {
            Ok(request) => match request.method.as_str() {
                "health_check" => SkillResponse {
                    id: request.id,
                    payload: SkillResponsePayload::Health(skill.health_check()),
                },

                "available_methods" => SkillResponse {
                    id: request.id,
                    payload: SkillResponsePayload::Methods(skill.available_methods()),
                },

                "execute" => SkillResponse {
                    id: request.id,
                    payload: match request.params {
                        Some(params) => match serde_json::from_str::<ExecuteParams>(&params) {
                            Ok(parameters) => {
                                match skill.execute(&parameters.method, &parameters.args).await {
                                    Ok(output) => SkillResponsePayload::Output(output),
                                    Err(error) => SkillResponsePayload::Error(error),
                                }
                            }

                            Err(error) => SkillResponsePayload::Error(SkillError::InvalidArgs(
                                error.to_string(),
                            )),
                        },
                        None => SkillResponsePayload::Error(SkillError::InvalidArgs(
                            "Not provided any parameters".to_string(),
                        )),
                    },
                },

                "get_metadata" => SkillResponse {
                    id: request.id,
                    payload: SkillResponsePayload::Metadata(skill.get_metadata().clone()),
                },

                _ => SkillResponse {
                    id: request.id,
                    payload: SkillResponsePayload::Error(SkillError::NotFound(
                        "Method not found".to_string(),
                    )),
                },
            },
            Err(error) => SkillResponse {
                id: 0,
                payload: SkillResponsePayload::Error(SkillError::InvalidArgs(error.to_string())),
            },
        };

        let json = serde_json::to_string(&response)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        out.write_all(json.as_bytes()).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
    }

    Ok(())
}
