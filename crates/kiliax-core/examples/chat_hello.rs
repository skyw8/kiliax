use kiliax_core::{
    config,
    llm::client_from_config,
    protocol::{ChatRequest, Message, UserMessageContent},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = config::load()?;
    let llm = client_from_config(&loaded.config, None)?;
    println!("Using model: {}", llm.route().model);

    let resp = llm
        .chat(ChatRequest::new(vec![Message::User {
            content: UserMessageContent::Text("hi".into()),
        }]))
        .await?;

    println!("Response: {:?}", resp);
    Ok(())
}
