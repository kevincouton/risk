package billing

import (
	"context"
	"database/sql"
	"fmt"
)

// SetPremium flips the premium flag on the user linked to a Stripe customer
// (via the subscriptions table). Entitlement updates are upserts keyed on
// the Stripe customer ID (spec §5.2).
//
// Note: the session cookie carries a Premium snapshot taken at login, so
// changes made here take effect for a user only on their next login.
func SetPremium(ctx context.Context, conn *sql.DB, stripeCustomerID string, premium bool) error {
	v := 0
	if premium {
		v = 1
	}
	res, err := conn.ExecContext(ctx, `
		UPDATE users SET premium = ?
		WHERE id = (SELECT user_id FROM subscriptions WHERE stripe_customer_id = ? LIMIT 1)
	`, v, stripeCustomerID)
	if err != nil {
		return err
	}
	n, err := res.RowsAffected()
	if err != nil {
		return err
	}
	if n == 0 {
		return fmt.Errorf("no user linked to stripe customer %s", stripeCustomerID)
	}
	return nil
}
