//! One authorization policy for UDS, HTTP and MCP callers.
use crate::{
    Error, Result,
    contracts::{Principal, Role},
};
pub fn local_owner() -> Principal {
    Principal {
        actor_id: "local-owner".into(),
        role: Role::Admin,
        namespaces: vec!["*".into()],
        capabilities: vec!["*".into()],
    }
}
pub fn namespace(principal: &Principal, namespace: &str) -> Result<()> {
    if principal
        .namespaces
        .iter()
        .any(|n| n == "*" || n == namespace)
    {
        Ok(())
    } else {
        Err(denied())
    }
}
pub fn has(principal: &Principal, capability: &str, target: Option<&str>) -> bool {
    principal.capabilities.iter().any(|c| {
        c == "*"
            || c == capability
            || target.is_some_and(|target| c == &format!("{capability}:{target}"))
    })
}
pub fn require(principal: &Principal, capability: &str, target: Option<&str>) -> Result<()> {
    let role_allowed = match capability {
        "workload.read" | "execution.read" => true,
        "logs.read" => true,
        "workload.run_preapproved" | "service.control_preapproved" => {
            matches!(principal.role, Role::Operator | Role::Editor | Role::Admin)
        }
        "workload.edit_safe_config" | "executor.create_command" => {
            matches!(principal.role, Role::Editor | Role::Admin)
        }
        "docker.manage" | "systemd.manage" => {
            matches!(principal.role, Role::Operator | Role::Editor | Role::Admin)
        }
        "tokens.manage" | "operations.manage" => principal.role == Role::Admin,
        _ => false,
    };
    let base =
        matches!(capability, "workload.read" | "execution.read") && principal.role != Role::Observe;
    if role_allowed && (base || has(principal, capability, target)) {
        Ok(())
    } else {
        Err(denied())
    }
}
pub fn denied() -> Error {
    Error::new(
        "NOT_AUTHORIZED",
        "The authenticated identity is not authorized for this operation",
    )
}
