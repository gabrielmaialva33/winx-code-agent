//! Tests for NVIDIA integration

#[cfg(test)]
mod tests {
    use crate::nvidia::{config::NvidiaConfig, models::*};

    #[test]
    fn test_nvidia_model_as_str() {
        assert_eq!(
            NvidiaModel::Llama31_70B.as_str(),
            "meta/llama-3.1-70b-instruct"
        );
        assert_eq!(
            NvidiaModel::Nemotron340B.as_str(),
            "nvidia/nemotron-4-340b-instruct"
        );
        assert_eq!(NvidiaModel::CodeGemma7B.as_str(), "google/codegemma-7b");
    }

    #[test]
    fn test_model_for_task() {
        assert_eq!(
            NvidiaModel::for_task(TaskType::CodeGeneration).as_str(),
            "qwen/qwen3-235b-a22b"
        );
        assert_eq!(
            NvidiaModel::for_task(TaskType::CodeAnalysis).as_str(),
            "qwen/qwen3-235b-a22b"
        );
        assert_eq!(
            NvidiaModel::for_task(TaskType::ComplexReasoning).as_str(),
            "qwen/qwen3-235b-a22b"
        );
        assert_eq!(
            NvidiaModel::for_task(TaskType::FastResponse).as_str(),
            "microsoft/phi-3-medium-128k-instruct"
        );
    }

    #[test]
    fn test_chat_message_creation() {
        let system_msg = ChatMessage::system("You are a helpful assistant");
        assert_eq!(system_msg.role, "system");
        assert_eq!(system_msg.content, "You are a helpful assistant");

        let user_msg = ChatMessage::user("Hello, world!");
        assert_eq!(user_msg.role, "user");
        assert_eq!(user_msg.content, "Hello, world!");

        let assistant_msg = ChatMessage::assistant("Hello! How can I help you?");
        assert_eq!(assistant_msg.role, "assistant");
        assert_eq!(assistant_msg.content, "Hello! How can I help you?");
    }

    #[test]
    fn test_issue_severity_display() {
        assert_eq!(format!("{}", IssueSeverity::Info), "Info");
        assert_eq!(format!("{}", IssueSeverity::Warning), "Warning");
        assert_eq!(format!("{}", IssueSeverity::Error), "Error");
        assert_eq!(format!("{}", IssueSeverity::Critical), "Critical");
    }

    #[test]
    fn test_issue_category_display() {
        assert_eq!(format!("{}", IssueCategory::Bug), "Bug");
        assert_eq!(format!("{}", IssueCategory::Performance), "Performance");
        assert_eq!(format!("{}", IssueCategory::Security), "Security");
        assert_eq!(format!("{}", IssueCategory::Style), "Style");
        assert_eq!(
            format!("{}", IssueCategory::Maintainability),
            "Maintainability"
        );
        assert_eq!(format!("{}", IssueCategory::Documentation), "Documentation");
    }

    #[test]
    fn test_nvidia_config_default() {
        let config = NvidiaConfig::default();
        assert_eq!(config.base_url, "https://integrate.api.nvidia.com");
        assert_eq!(config.default_model, "qwen/qwen3-235b-a22b");
        assert_eq!(config.timeout_seconds, 30);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.rate_limit_rpm, 60);
        assert!(config.api_key.is_empty());
    }

    #[test]
    fn test_nvidia_config_validation() {
        let mut config = NvidiaConfig::default();

        // Empty API key should fail validation
        assert!(config.validate().is_err());

        // Valid config should pass
        config.api_key = "test-api-key".to_string();
        assert!(config.validate().is_ok());

        // Empty base URL should fail
        config.base_url = String::new();
        assert!(config.validate().is_err());

        // Zero timeout should fail
        config.base_url = "https://integrate.api.nvidia.com".to_string();
        config.timeout_seconds = 0;
        assert!(config.validate().is_err());

        // Zero rate limit should fail
        config.timeout_seconds = 30;
        config.rate_limit_rpm = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_chat_completions_url() {
        let config = NvidiaConfig::default();
        assert_eq!(
            config.chat_completions_url(),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );

        let mut config_with_trailing_slash = config;
        config_with_trailing_slash.base_url = "https://integrate.api.nvidia.com/".to_string();
        assert_eq!(
            config_with_trailing_slash.chat_completions_url(),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_code_generation_request() {
        let request = CodeGenerationRequest {
            prompt: "Create a hello world function".to_string(),
            language: Some("Rust".to_string()),
            context: Some("This is for testing".to_string()),
            max_tokens: Some(100),
            temperature: Some(0.7),
        };

        assert_eq!(request.prompt, "Create a hello world function");
        assert_eq!(request.language, Some("Rust".to_string()));
        assert_eq!(request.context, Some("This is for testing".to_string()));
        assert_eq!(request.max_tokens, Some(100));
        assert_eq!(request.temperature, Some(0.7));
    }

    #[test]
    fn test_code_analysis_result() {
        let analysis = CodeAnalysisResult {
            summary: "Code looks good".to_string(),
            issues: vec![],
            suggestions: vec!["Add more comments".to_string()],
            complexity_score: Some(25),
        };

        assert_eq!(analysis.summary, "Code looks good");
        assert!(analysis.issues.is_empty());
        assert_eq!(analysis.suggestions.len(), 1);
        assert_eq!(analysis.complexity_score, Some(25));
    }

    #[test]
    fn test_code_issue() {
        let issue = CodeIssue {
            severity: IssueSeverity::Warning,
            category: IssueCategory::Style,
            message: "Variable name should be snake_case".to_string(),
            line: Some(42),
            suggestion: Some("Rename camelCase to snake_case".to_string()),
        };

        assert_eq!(format!("{}", issue.severity), "Warning");
        assert_eq!(format!("{}", issue.category), "Style");
        assert_eq!(issue.message, "Variable name should be snake_case");
        assert_eq!(issue.line, Some(42));
        assert_eq!(
            issue.suggestion,
            Some("Rename camelCase to snake_case".to_string())
        );
    }
}
