use ruma::{
    api::client::{error::ErrorKind, room::report_content},
    events::room::message,
    int,
};

use crate::{services, Ar, Error, Ra, Result};

/// # `POST /_matrix/client/r0/rooms/{roomId}/report/{eventId}`
///
/// Reports an inappropriate event to homeserver admins
pub(crate) async fn report_event_route(
    body: Ar<report_content::v3::Request>,
) -> Result<Ra<report_content::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let Some(pdu) = services().rooms.timeline.get_pdu(&body.event_id)? else {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "Invalid Event ID",
        ));
    };

    if let Some(true) = body.score.map(|s| s > int!(0) || s < int!(-100)) {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "Invalid score, must be within 0 to -100",
        ));
    };

    if let Some(true) = body.reason.clone().map(|s| s.chars().count() > 250) {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "Reason too long, should be 250 characters or fewer",
        ));
    };

    services().admin.send_message(message::RoomMessageEventContent::text_html(
        format!(
            "Report received from: {}\n\nEvent ID: {:?}\nRoom ID: {:?}\nSent \
             By: {:?}\n\nReport Score: {:?}\nReport Reason: {:?}",
            sender_user,
            pdu.event_id,
            pdu.room_id,
            pdu.sender,
            body.score,
            body.reason
        ),
        format!(
            r#"
            <details>
                <summary>
                    Report received from:
                    <a href="https://matrix.to/#/{0:?}">{0:?}</a>
                </summary>
                <ul>
                    <li>
                        Event Info
                        <ul>
                            <li>
                                Event ID:
                                <code>{1:?}</code>
                                <a href="https://matrix.to/#/{2:?}/{1:?}">🔗</a>
                            </li>
                            <li>
                                Room ID:
                                <code>{2:?}</code>
                            </li>
                            <li>
                                Sent By:
                                <a href="https://matrix.to/#/{3:?}">{3:?}</a>
                            </li>
                        </ul>
                    </li>
                    <li>
                        Report Info
                        <ul>
                            <li>Report Score: {4:?}</li>
                            <li>Report Reason: {5}</li>
                        </ul>
                    </li>
                </ul>
            </details>
            "#,
            sender_user,
            pdu.event_id,
            pdu.room_id,
            pdu.sender,
            body.score,
            html_escape::encode_safe(body.reason.as_deref().unwrap_or(""))
        ),
    ));

    Ok(Ra(report_content::v3::Response {}))
}
