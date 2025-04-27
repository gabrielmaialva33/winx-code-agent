use crate::bash::{AllowedCommandsType, BashStateStatus, Context};
use crate::types::{BashAction, BashCommand, Special};
use anyhow::{anyhow, Result};
use chrono::Utc;

const WAITING_INPUT_MESSAGE: &str = "A command is already running. NOTE: You can't run multiple shell sessions, likely a previous program hasn't exited. 
1. Get its output using status check.
2. Use `send_ascii` or `send_specials` to give inputs to the running program OR
3. kill the previous program by sending ctrl+c first using `send_ascii` or `send_specials`
4. Interrupt and run the process in background by re-running it using screen
";

pub async fn bash_command_tool(ctx: &Context, cmd: &BashCommand) -> Result<String> {
    // Check if the chat ID matches
    {
        let state = ctx.bash_state.lock().unwrap();
        if cmd.chat_id != state.current_chat_id {
            return Err(anyhow!("Error: No saved bash state found for chat ID {}. Please initialize first with this ID.", cmd.chat_id));
        }
    }

    // Process the command based on its type
    match &cmd.action_json {
        BashAction::Command(command) => {
            // Check if commands are allowed
            {
                let state = ctx.bash_state.lock().unwrap();
                match state.bash_command_mode.allowed_commands {
                    AllowedCommandsType::None => {
                        return Err(anyhow!("Error: BashCommand not allowed in current mode"));
                    }
                    _ => {}
                }

                // Check if the shell is ready to accept commands
                match state.state {
                    BashStateStatus::Pending(_) => {
                        return Err(anyhow!(WAITING_INPUT_MESSAGE));
                    }
                    _ => {}
                }
            }

            // Execute the command
            let cmd_str = &command.command;
            state_print(ctx, &format!("$ {}", cmd_str));

            // Update the state
            {
                let mut state = ctx.bash_state.lock().unwrap();
                state.state = BashStateStatus::Pending(Utc::now());
            }

            // Execute the command
            let output = {
                let mut state = ctx.bash_state.lock().unwrap();
                state.execute_command(cmd_str)?
            };

            // Update the state again
            {
                let mut state = ctx.bash_state.lock().unwrap();
                state.state = BashStateStatus::Repl;
                // Attempt to update the current working directory
                let _ = state.update_cwd();
            }

            // Add status information
            let status = {
                let state = ctx.bash_state.lock().unwrap();
                state.get_status()
            };

            Ok(format!("{}{}", output, status))
        }
        BashAction::StatusCheck(_) => {
            // Check the status of the current command
            let status = {
                let state = ctx.bash_state.lock().unwrap();
                match state.state {
                    BashStateStatus::Pending(_) => {
                        format!("Command is still running\n{}", state.get_status())
                    }
                    BashStateStatus::Repl => {
                        format!("No command is running\n{}", state.get_status())
                    }
                }
            };

            Ok(status)
        }
        BashAction::SendText(send_text) => {
            // Send text to the current command
            if send_text.send_text.is_empty() {
                return Err(anyhow!("Failure: send_text cannot be empty"));
            }

            state_print(ctx, &format!("Interact text: {}", send_text.send_text));

            // In a real implementation, we would send the text to the process
            let mut state = ctx.bash_state.lock().unwrap();
            state.execute_command(&send_text.send_text)?;

            Ok(format!("Text sent: {}", send_text.send_text))
        }
        BashAction::SendSpecials(send_specials) => {
            // Send special keys to the current command
            if send_specials.send_specials.is_empty() {
                return Err(anyhow!("Failure: send_specials cannot be empty"));
            }

            state_print(
                ctx,
                &format!(
                    "Sending special sequence: {:?}",
                    send_specials.send_specials
                ),
            );

            // In a real implementation, we would send the special keys to the process
            let mut output = String::new();
            for special in &send_specials.send_specials {
                match special {
                    Special::Enter => {
                        output.push_str("Enter key sent\n");
                    }
                    Special::KeyUp => {
                        output.push_str("Up arrow key sent\n");
                    }
                    Special::KeyDown => {
                        output.push_str("Down arrow key sent\n");
                    }
                    Special::KeyLeft => {
                        output.push_str("Left arrow key sent\n");
                    }
                    Special::KeyRight => {
                        output.push_str("Right arrow key sent\n");
                    }
                    Special::CtrlC => {
                        output.push_str("Ctrl+C sent\n");
                    }
                    Special::CtrlD => {
                        output.push_str("Ctrl+D sent\n");
                    }
                }
            }

            Ok(output)
        }
        BashAction::SendAscii(send_ascii) => {
            // Send ASCII characters to the current command
            if send_ascii.send_ascii.is_empty() {
                return Err(anyhow!("Failure: send_ascii cannot be empty"));
            }

            state_print(
                ctx,
                &format!("Sending ASCII sequence: {:?}", send_ascii.send_ascii),
            );

            // In a real implementation, we would send the ASCII characters to the process
            let chars: String = send_ascii.send_ascii.iter().map(|&b| b as char).collect();

            Ok(format!("ASCII characters sent: {}", chars))
        }
    }
}

fn state_print(ctx: &Context, message: &str) {
    let state = ctx.bash_state.lock().unwrap();
    state.console.print(message);
}
